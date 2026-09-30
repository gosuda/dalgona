//! ACP `initialize`: branch selection and the byte-exact result bodies.

use std::fmt::Write as _;
use std::sync::Arc;

use sonic_rs::{JsonValueTrait, Value};
use tokio::sync::Mutex;

use super::{AcpConn, AcpVersion};
use crate::jsonrpc::{Id, Message};
use crate::transport::FrameWriter;

/// Handles `initialize`: selects the branch and answers the byte-exact result.
pub(super) async fn initialize(
    state: &Arc<Mutex<AcpConn>>,
    writer: &FrameWriter,
    id: &Id,
    params: &Value,
) -> Option<Message> {
    let version = params
        .get("protocolVersion")
        .and_then(Value::as_i64)
        .unwrap_or(0);
    let selected = match version {
        1 => AcpVersion::V1,
        _ => AcpVersion::V2,
    };
    let name = params
        .get("clientInfo")
        .and_then(|info| info.get("name"))
        .and_then(|name| name.as_str())
        .filter(|name| !name.is_empty())
        .or_else(|| {
            params
                .get("info")
                .and_then(|info| info.get("name"))
                .and_then(|name| name.as_str())
                .filter(|name| !name.is_empty())
        })
        .unwrap_or("acp")
        .to_owned();
    let elicitation = params
        .get("clientCapabilities")
        .and_then(|capabilities| capabilities.get("elicitation"))
        .and_then(|elicitation| elicitation.get("form"))
        .is_some_and(|form| !form.is_null() && form.as_bool() != Some(false));
    {
        let mut locked = state.lock().await;
        locked.version = selected;
        locked.initialized = true;
        locked.client = crate::protocol::mint_client_id(&name);
        locked.elicitation_form = elicitation && selected == AcpVersion::V1;
    }
    let body = match selected {
        AcpVersion::V1 => v1_init_body(),
        AcpVersion::V2 => v2_init_body(),
    };
    let id_text = match id {
        Id::String(text) => json_string(text),
        Id::Integer(number) => number.to_string(),
        Id::Null => "null".to_owned(),
    };
    let frame = format!("{{\"jsonrpc\":\"2.0\",\"id\":{id_text},\"result\":{body}}}");
    if writer.write_frame(&frame).await.is_err() {
        tracing::debug!("initialize reply write failed");
    }
    None
}

/// Encodes one JSON string without a serializer.
fn json_string(text: &str) -> String {
    let mut encoded = String::with_capacity(text.len() + 2);
    encoded.push('"');
    for ch in text.chars() {
        match ch {
            '"' => encoded.push_str("\\\""),
            '\\' => encoded.push_str("\\\\"),
            '\n' => encoded.push_str("\\n"),
            '\r' => encoded.push_str("\\r"),
            '\t' => encoded.push_str("\\t"),
            ch if u32::from(ch) < 0x20 => {
                let _ = write!(encoded, "\\u{:04x}", u32::from(ch));
            }
            ch => encoded.push(ch),
        }
    }
    encoded.push('"');
    encoded
}

/// Builds the byte-exact v1 `initialize` result.
fn v1_init_body() -> String {
    format!(
        "{{\"protocolVersion\":1,\"agentCapabilities\":{{\"loadSession\":true,\"promptCapabilities\":{{\"image\":true,\"audio\":false,\"embeddedContext\":true}},\"mcpCapabilities\":{{\"http\":false,\"sse\":false}},\"sessionCapabilities\":{{\"list\":{{}},\"resume\":{{}},\"close\":{{}}}}}},\"authMethods\":[],\"agentInfo\":{{\"name\":\"dal\",\"title\":\"dal\",\"version\":\"{}\"}}}}",
        crate::rpc::crate_version()
    )
}

/// Builds the byte-exact v2 `initialize` result.
fn v2_init_body() -> String {
    format!(
        "{{\"protocolVersion\":2,\"capabilities\":{{\"session\":{{\"prompt\":{{\"image\":{{}},\"embeddedContext\":{{}}}}}}}},\"info\":{{\"name\":\"dal\",\"title\":\"dal\",\"version\":\"{}\"}},\"authMethods\":[]}}",
        crate::rpc::crate_version()
    )
}
