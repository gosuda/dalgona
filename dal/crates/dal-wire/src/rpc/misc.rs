//! Non-session method handlers for version-1 RPC: blobs, commands, models,
//! docs, and host subscriptions. Auth lives in `auth`.

use std::sync::Arc;

use dal_agent::Host;
use dal_core::{Family, ModelRoute};
use serde::Serialize;
use sonic_rs::Value;
use tokio::sync::Mutex;

use super::{Conn, host_error, host_notifier, invalid_params, opt_string, to_value};
use crate::jsonrpc::{ErrorObject, Id, Message};
use crate::transport::FrameWriter;

/// Handles `blob/read`: returns one blob as base64 with its media type.
pub(crate) async fn blob_read(
    host: &Host,
    state: &Arc<Mutex<Conn>>,
    params: &Value,
) -> Result<Value, ErrorObject> {
    let id = super::session::session_param("blob/read", params)?;
    let raw = opt_string(params, "blobId")
        .ok_or_else(|| invalid_params("blob/read", "missing member `blobId`"))?;
    let blob = dal_core::BlobId::parse(&raw)
        .map_err(|_| invalid_params("blob/read", "blobId is not valid"))?;
    let agent = super::session::agent_for(host, state, id).await?;
    let bytes = agent
        .blob(blob)
        .await
        .map_err(|error| super::agent_error("blob/read", error))?;
    let mime = sniff_mime(&bytes);
    let encoded = base64::Engine::encode(&base64::engine::general_purpose::STANDARD, &bytes);
    Ok(sonic_rs::json!({"mimeType": mime, "base64": encoded}))
}

/// Recovers a blob media type from its magic bytes.
///
/// The store knows the recorded mime, but the `Agent` read path does not
/// expose it yet (host seam requested). This table only reports types the
/// bytes prove: PNG, JPEG, GIF, WebP, PDF, UTF-8 text, or opaque bytes.
fn sniff_mime(bytes: &[u8]) -> &'static str {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        "image/png"
    } else if bytes.starts_with(b"\xff\xd8\xff") {
        "image/jpeg"
    } else if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        "image/gif"
    } else if bytes.len() >= 12 && bytes.starts_with(b"RIFF") && bytes[8..12] == *b"WEBP" {
        "image/webp"
    } else if bytes.starts_with(b"%PDF-") {
        "application/pdf"
    } else if std::str::from_utf8(bytes).is_ok() {
        "text/plain"
    } else {
        "application/octet-stream"
    }
}

/// One `commands/list` row.
#[derive(Serialize)]
struct CommandRow<'a> {
    name: &'a str,
    summary: &'a str,
    args: Option<&'a str>,
}

/// Handles `commands/list`: returns the merged command table.
pub(crate) fn commands_list(host: &Host, params: &Value) -> Value {
    let _ = params;
    let specs = host.commands();
    let rows: Vec<CommandRow<'_>> = specs
        .iter()
        .map(|spec| CommandRow {
            name: spec.name.as_str(),
            summary: &spec.summary,
            args: spec.args_hint.as_deref(),
        })
        .collect();
    let commands = super::to_value(&rows).unwrap_or_else(|_| sonic_rs::json!([]));
    sonic_rs::json!({"commands": commands})
}

/// One `models/list` row.
#[derive(Serialize)]
struct ModelRow {
    id: String,
    provider: String,
    name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(rename = "contextWindow")]
    context_window: Option<u32>,
}

/// Handles `models/list`: lists display rows without a network round trip
/// beyond the host's own catalog refresh.
pub(crate) async fn models_list(host: &Host, params: &Value) -> Result<Value, ErrorObject> {
    let _ = params;
    let infos = host.models(None).await.map_err(host_error)?;
    let mut rows: Vec<ModelRow> = infos
        .into_iter()
        .map(|info| {
            let (id, provider) = route_identity(&info.route);
            ModelRow {
                id,
                provider,
                name: info.name.into_string(),
                context_window: info.caps.context_window,
            }
        })
        .collect();
    rows.sort_by(|left, right| left.id.as_bytes().cmp(right.id.as_bytes()));
    let models = to_value(&rows)?;
    Ok(sonic_rs::json!({"models": models}))
}

/// Splits one route into its wire id and provider label.
pub(super) fn route_identity(route: &ModelRoute) -> (String, String) {
    match route {
        ModelRoute::Synthetic { id } => {
            let provider = id.split('/').next().unwrap_or("").to_owned();
            (id.to_string(), provider)
        }
        ModelRoute::Api { family, model } => {
            let provider = match family {
                Family::Chat | Family::Responses => "openai",
                Family::Codex => "openai-codex",
                Family::Anthropic => "anthropic",
            }
            .to_owned();
            (model.to_string(), provider)
        }
        ModelRoute::Harness { id } => (id.to_string(), "dalgon".to_owned()),
    }
}

/// One `docs/read` list row.
#[derive(Serialize)]
struct DocumentRow {
    uri: String,
    title: String,
}

/// Handles `docs/read`: reads one document, or lists known documents.
pub(crate) fn docs_read(host: &Host, params: &Value) -> Result<Value, ErrorObject> {
    match opt_string(params, "uri") {
        None => {
            let rows: Vec<DocumentRow> = host
                .docs()
                .into_iter()
                .map(|entry| DocumentRow {
                    uri: entry.uri.into_string(),
                    title: entry.title.into_string(),
                })
                .collect();
            let documents = to_value(&rows)?;
            Ok(sonic_rs::json!({"documents": documents}))
        }
        Some(uri) => {
            let doc = host
                .doc(&uri)
                .map_err(|error| super::scheme_error(&uri, error))?;
            Ok(sonic_rs::json!({"uri": doc.uri.into_string(), "text": doc.text.into_string()}))
        }
    }
}

/// Handles `host/subscribe`: replies `{}`, then pumps host updates.
///
/// The reply is written inline before any notification; this handler returns
/// `None` so the dispatcher sends no second reply.
pub(crate) async fn host_subscribe(
    host: &Host,
    state: &Arc<Mutex<Conn>>,
    writer: &FrameWriter,
    id: &Id,
    params: &Value,
) -> Option<Message> {
    let _ = params;
    host_notifier(host.clone(), state.clone(), writer.clone(), id).await
}

/// Handles `host/unsubscribe`: stops the host notifier.
pub(crate) async fn host_unsubscribe(
    state: &Arc<Mutex<Conn>>,
    params: &Value,
) -> Result<Value, ErrorObject> {
    let _ = params;
    let mut locked = state.lock().await;
    if let Some(token) = locked.host_sub.take() {
        token.cancel();
    }
    Ok(sonic_rs::json!({}))
}
