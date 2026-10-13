//! ACP `session/update` and `_dal/notice` notifications.

use dal_agent::Host;
use dal_core::SessionId;
use sonic_rs::{JsonContainerTrait, JsonValueTrait, Value};

use super::AcpVersion;
use crate::jsonrpc::Message;
use crate::transport::FrameWriter;

/// Sends one `session/update` notification carrying a mapped update.
pub(crate) async fn send_update(writer: &FrameWriter, session: SessionId, update: Value) {
    crate::rpc::send(
        writer,
        &Message::Notification {
            method: "session/update".to_owned(),
            params: sonic_rs::json!({"sessionId": session.to_string(), "update": update}),
        },
    )
    .await;
}

/// Sends one `_dal/notice` notification.
pub(crate) async fn send_notice(
    writer: &FrameWriter,
    session: SessionId,
    kind: &str,
    text: String,
) {
    send_update(
        writer,
        session,
        sonic_rs::json!({
            "_dal/notice": {"sessionId": session.to_string(), "kind": kind, "text": text},
        }),
    )
    .await;
}

/// Emits the `available_commands_update` after `session/new`.
pub(crate) async fn send_commands_update(
    host: &Host,
    version: AcpVersion,
    writer: &FrameWriter,
    session: SessionId,
) {
    let mut commands = Vec::new();
    for spec in host.commands().iter() {
        let hint = spec.args_hint.as_deref().unwrap_or("");
        let input = match version {
            AcpVersion::V1 => sonic_rs::json!({"hint": hint}),
            AcpVersion::V2 => sonic_rs::json!({"type": "unstructured", "hint": hint}),
        };
        commands.push(sonic_rs::json!({
            "name": spec.name.as_str(),
            "description": spec.summary.as_ref(),
            "input": input,
        }));
    }
    send_update(
        writer,
        session,
        sonic_rs::json!({
            "available_commands_update": {
                "sessionId": session.to_string(),
                "availableCommands": commands,
            },
        }),
    )
    .await;
}

/// Warns once about ignored `mcpServers`.
pub(crate) async fn note_mcp_servers(writer: &FrameWriter, session: SessionId, params: &Value) {
    let count = params
        .get("mcpServers")
        .and_then(|value| value.as_array())
        .map_or(0, sonic_rs::Array::len);
    if count == 0 {
        return;
    }
    send_notice(
        writer,
        session,
        "info",
        format!("dalgon does not load MCP servers: {count} ignored"),
    )
    .await;
}
