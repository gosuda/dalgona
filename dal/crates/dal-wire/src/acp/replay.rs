//! ACP history replay for `session/load` and v2 `replayFrom: start`.
//!
//! The leaf path is paged from the head view 200 entries at a time, then
//! replayed root-first. Each entry id becomes the replayed message id; only
//! text parts and text blocks replay, and tool results and control entries
//! are skipped.

use std::num::NonZeroU32;

use dal_agent::Agent;
use dal_core::{EntryView, PageReq, SessionId};
use sonic_rs::{JsonContainerTrait, JsonValueTrait, Value};

use super::send_update;
use crate::jsonrpc::ErrorObject;
use crate::transport::FrameWriter;

/// Entries per replay page.
const REPLAY_PAGE: u32 = 200;

/// Replays one session's leaf history root-first.
pub(crate) async fn replay_history(
    agent: &Agent,
    session: SessionId,
    writer: &FrameWriter,
) -> Result<(), ErrorObject> {
    let limit = NonZeroU32::new(REPLAY_PAGE).unwrap_or(NonZeroU32::MIN);
    let mut before = None;
    let mut pages: Vec<Vec<EntryView>> = Vec::new();
    loop {
        let page = PageReq::new(limit, before).map_err(|_| ErrorObject {
            code: -32603,
            message: "internal error: replay page was rejected".to_owned(),
            data: Some(crate::rpc::hint_value()),
        })?;
        let view = agent
            .view(page)
            .map_err(|error| crate::rpc::agent_error("session/load", error))?;
        pages.push(view.entries.items);
        match view.entries.next_before {
            Some(cursor) => before = Some(cursor),
            None => break,
        }
    }
    for entry in pages.iter().rev().flatten() {
        replay_entry(session, writer, entry).await;
    }
    Ok(())
}

/// Replays one entry as a user or agent message; other kinds are skipped.
async fn replay_entry(session: SessionId, writer: &FrameWriter, entry: &EntryView) {
    let Ok(kind) = sonic_rs::to_value(&entry.kind) else {
        return;
    };
    let entry = entry.id.to_string();
    let tag = kind
        .get("type")
        .and_then(|value| value.as_str())
        .unwrap_or("");
    match tag {
        "user" => {
            let text = join_text_parts(kind.get("parts"));
            send_update(
                writer,
                session,
                sonic_rs::json!({
                    "sessionUpdate": "user_message",
                    "messageId": entry,
                    "content": [{"type": "text", "text": text}],
                }),
            )
            .await;
        }
        "assistant" => {
            let text = join_text_blocks(kind.get("content"));
            if !text.is_empty() {
                send_update(
                    writer,
                    session,
                    sonic_rs::json!({
                        "sessionUpdate": "agent_message_chunk",
                        "messageId": entry,
                        "content": {"type": "text", "text": text},
                    }),
                )
                .await;
            }
        }
        _ => {}
    }
}

/// Joins the text members of journal parts.
fn join_text_parts(parts: Option<&Value>) -> String {
    let mut out = String::new();
    let items = parts.and_then(|value| value.as_array());
    let Some(items) = items else {
        return out;
    };
    for part in items {
        if part.get("type").and_then(|value| value.as_str()) == Some("text")
            && let Some(text) = part.get("text").and_then(|value| value.as_str())
        {
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str(text);
        }
    }
    out
}

/// Joins the text members of assistant content blocks.
fn join_text_blocks(blocks: Option<&Value>) -> String {
    let mut out = String::new();
    let items = blocks.and_then(|value| value.as_array());
    let Some(items) = items else {
        return out;
    };
    for block in items {
        if block.get("type").and_then(|value| value.as_str()) == Some("text")
            && let Some(text) = block.get("text").and_then(|value| value.as_str())
        {
            out.push_str(text);
        }
    }
    out
}
