//! ACP version 1 branch dispatch.
//!
//! V1 is request/response turn completion: `session/prompt` answers
//! `{"stopReason"}` after the prompt turn's last update, and a failed turn
//! answers `-32010` with the failure message and hint. Arrays are never
//! batches.

use std::sync::Arc;

use dal_agent::Host;
use dal_core::SessionId;
use sonic_rs::{JsonValueMutTrait, JsonValueTrait, Value};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

use super::map::{PromptEnd, PumpCtx, prompt_pump, stop_literal};
use super::{
    AcpConn, AcpVersion, agent_for, check_idle, check_model, note_mcp_servers, open_workspace,
    prompt_parts, send_commands_update, submit_prompt, view_head,
};
use crate::jsonrpc::{ErrorObject, Id, Message};
use crate::transport::FrameWriter;

/// Dispatches one v1 request.
pub(crate) async fn dispatch(
    host: &Host,
    state: &Arc<Mutex<AcpConn>>,
    writer: &FrameWriter,
    id: &Id,
    method: &str,
    params: &Value,
    cancel: &CancellationToken,
) -> Option<Message> {
    let ok = |result: Value| {
        Some(Message::Result {
            id: id.clone(),
            result,
        })
    };
    let fail = |error: ErrorObject| {
        Some(Message::Error {
            id: id.clone(),
            error,
        })
    };
    match method {
        "session/new" => new_session(host, state, writer, id, params).await,
        "session/load" => load_session(host, state, writer, params)
            .await
            .map_or_else(fail, ok),
        "session/resume" => resume_session(host, state, params)
            .await
            .map_or_else(fail, ok),
        "session/list" => list_sessions(host, params).await.map_or_else(fail, ok),
        "session/close" => close_session(host, state, params)
            .await
            .map_or_else(fail, ok),
        "session/prompt" => prompt(host, state, writer, id, params, cancel).await,
        _ => fail(ErrorObject {
            code: -32601,
            message: format!(r#"unknown method "{method}""#),
            data: None,
        }),
    }
}

/// Handles v1 `session/new`: opens a workspace session, answers its id, then
/// sends the commands update and the MCP notice in order.
async fn new_session(
    host: &Host,
    state: &Arc<Mutex<AcpConn>>,
    writer: &FrameWriter,
    id: &Id,
    params: &Value,
) -> Option<Message> {
    let cwd = match crate::rpc::req_string("session/new", params, "cwd") {
        Ok(cwd) => cwd,
        Err(error) => {
            return Some(Message::Error {
                id: id.clone(),
                error,
            });
        }
    };
    let (session, _) = match open_workspace(host, state, &cwd).await {
        Ok(opened) => opened,
        Err(error) => {
            return Some(Message::Error {
                id: id.clone(),
                error,
            });
        }
    };
    crate::rpc::send(
        writer,
        &Message::Result {
            id: id.clone(),
            result: sonic_rs::json!({"sessionId": session.to_string()}),
        },
    )
    .await;
    send_commands_update(host, AcpVersion::V1, writer, session).await;
    note_mcp_servers(writer, session, params).await;
    None
}

/// Handles v1 `session/load`: replays the leaf history, then answers `{}`.
async fn load_session(
    host: &Host,
    state: &Arc<Mutex<AcpConn>>,
    writer: &FrameWriter,
    params: &Value,
) -> Result<Value, ErrorObject> {
    let session = session_param(params, "session/load")?;
    let agent = agent_for(host, state, session).await?;
    super::replay::replay_history(&agent, session, writer).await?;
    Ok(sonic_rs::json!({}))
}

/// Handles v1 `session/resume`: binds an existing session without replay.
async fn resume_session(
    host: &Host,
    state: &Arc<Mutex<AcpConn>>,
    params: &Value,
) -> Result<Value, ErrorObject> {
    let session = session_param(params, "session/resume")?;
    agent_for(host, state, session).await?;
    Ok(sonic_rs::json!({"sessionId": session.to_string()}))
}

/// Handles v1 `session/list`: pages sessions with exact workspace filtering.
async fn list_sessions(host: &Host, params: &Value) -> Result<Value, ErrorObject> {
    list_shared(host, params, 50).await
}

/// Handles v1 `session/close`: cancels the running turn, then closes.
async fn close_session(
    host: &Host,
    state: &Arc<Mutex<AcpConn>>,
    params: &Value,
) -> Result<Value, ErrorObject> {
    close_shared(host, state, params).await
}

/// Handles v1 `session/prompt`: completes the turn, then answers.
///
/// The handler owns the cancel token: cancellation submits a turn cancel and
/// the response carries the cancelled stop reason.
async fn prompt(
    host: &Host,
    state: &Arc<Mutex<AcpConn>>,
    writer: &FrameWriter,
    id: &Id,
    params: &Value,
    cancel: &CancellationToken,
) -> Option<Message> {
    let fail = |error: ErrorObject| {
        Some(Message::Error {
            id: id.clone(),
            error,
        })
    };
    let session = match session_param(params, "session/prompt") {
        Ok(session) => session,
        Err(error) => return fail(error),
    };
    let agent = match agent_for(host, state, session).await {
        Ok(agent) => agent,
        Err(error) => return fail(error),
    };
    let head = match view_head(&agent, "session/prompt").await {
        Ok(head) => head,
        Err(error) => return fail(error),
    };
    if let Err(error) = check_idle(&head) {
        return fail(error);
    }
    if let Err(error) = check_model(host, &head).await {
        return fail(error);
    }
    let blocks = match params.get("prompt") {
        Some(blocks) => blocks.clone(),
        None => {
            return fail(crate::rpc::invalid_params(
                "session/prompt",
                "missing member `prompt`",
            ));
        }
    };
    let parts = match prompt_parts(&blocks) {
        Ok(parts) => parts,
        Err(error) => return fail(error),
    };
    let subscription = match agent.subscribe(None) {
        Ok(subscription) => subscription,
        Err(error) => return fail(crate::rpc::agent_error("session/prompt", error)),
    };
    let (turn, _) = match submit_prompt(host, &agent, "session/prompt", parts).await {
        Ok(accepted) => accepted,
        Err(error) => return fail(error),
    };
    let ctx = PumpCtx {
        state,
        writer,
        agent: &agent,
        session,
        version: AcpVersion::V1,
        prompt_turn: turn,
    };
    match prompt_pump(&ctx, subscription, cancel).await {
        PromptEnd::Stopped(stop) => {
            let literal = stop_literal(stop);
            if let Some(failed) = literal.get("failed") {
                let message = failed
                    .get("message")
                    .and_then(|message| message.as_str())
                    .unwrap_or("the turn failed")
                    .to_owned();
                let hint = failed
                    .get("hint")
                    .cloned()
                    .unwrap_or(Value::from("report this"));
                Some(Message::Error {
                    id: id.clone(),
                    error: ErrorObject {
                        code: -32010,
                        message,
                        data: Some(sonic_rs::json!({"hint": hint})),
                    },
                })
            } else {
                Some(Message::Result {
                    id: id.clone(),
                    result: sonic_rs::json!({"stopReason": literal}),
                })
            }
        }
        PromptEnd::Failed(text) => fail(ErrorObject {
            code: -32010,
            message: text,
            data: Some(sonic_rs::json!({"hint": "report this"})),
        }),
    }
}

/// Reads the `sessionId` member shared by session methods.
fn session_param(params: &Value, method: &str) -> Result<SessionId, ErrorObject> {
    let text = crate::rpc::req_string(method, params, "sessionId")?;
    SessionId::parse(&text)
        .map_err(|_| crate::rpc::invalid_params(method, "sessionId is not valid"))
}

/// Lists sessions with exact workspace filtering; shared with v2.
pub(crate) async fn list_shared(
    host: &Host,
    params: &Value,
    limit: u32,
) -> Result<Value, ErrorObject> {
    let filter = crate::rpc::opt_nullable_string("session/list", params, "cwd")?;
    let query = dal_core::ListQuery {
        limit: Some(limit),
        cursor: crate::rpc::opt_nullable_string("session/list", params, "cursor")?
            .map(String::into_boxed_str),
        search: None,
    };
    let page = host.sessions(query).map_err(crate::rpc::host_error)?;
    // Session workspaces are recorded in the store's canonical spelling; the
    // caller's cwd canonicalizes to the same form before comparing.
    let sessions: Vec<Value> = page
        .items
        .into_iter()
        .filter(|info| {
            filter.as_deref().is_none_or(|cwd| {
                info.workspace.as_path()
                    == dal_store::canonical_path(std::path::Path::new(cwd)).as_path()
            })
        })
        .map(|info| {
            sonic_rs::json!({
                "sessionId": info.id.to_string(),
                "cwd": info.workspace.as_path().display().to_string(),
                "title": info.name.as_deref().unwrap_or(""),
                "updatedAt": info.updated_at.to_string(),
            })
        })
        .collect();
    let mut result = sonic_rs::json!({"sessions": sessions});
    if let Some(cursor) = page.next_before
        && let Some(object) = result.as_object_mut()
    {
        object.insert("nextCursor", Value::from(cursor.as_ref()));
    }
    Ok(result)
}

/// Closes one session after cancelling its running turn; shared with v2.
pub(crate) async fn close_shared(
    host: &Host,
    state: &Arc<Mutex<AcpConn>>,
    params: &Value,
) -> Result<Value, ErrorObject> {
    let session = session_param(params, "session/close")?;
    let held = state.lock().await.agents.get(&session).cloned();
    if let Some(agent) = held
        && let Ok(view) = agent.view(dal_core::PageReq::default())
    {
        let turn = match view.turn {
            dal_core::TurnState::Running { turn } | dal_core::TurnState::Settling { turn } => {
                Some(turn)
            }
            dal_core::TurnState::Idle | dal_core::TurnState::Compacting { .. } => None,
        };
        if let Some(turn) = turn {
            let cancel = dal_core::Command::Cancel {
                scope: dal_core::CancelScope::Turn(turn),
            };
            let _ = agent.submit(cancel).await;
        }
    }
    host.close(session).await.map_err(crate::rpc::host_error)?;
    let mut locked = state.lock().await;
    locked.agents.remove(&session);
    locked.opened.remove(&session);
    Ok(sonic_rs::json!({}))
}
