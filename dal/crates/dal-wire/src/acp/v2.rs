//! ACP version 2 branch dispatch.
//!
//! V2 answers `session/prompt` with `{"messageId"}` at acceptance, replays
//! the prompt as `user_message`, and completes with an idle `state_update`.
//! A failed turn sends a warning notice, then idle with `_dal_failed`.
//! Lifecycle methods are refused inside batches.

use std::sync::Arc;

use dal_agent::Host;
use dal_core::SessionId;
use sonic_rs::{JsonValueTrait, Value};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

use super::map::{PromptEnd, PumpCtx, prompt_pump, state_update_idle, stop_literal};
use super::v1::{close_shared, list_shared};
use super::{
    AcpConn, AcpVersion, agent_for, check_idle, check_model, note_mcp_servers, open_workspace,
    prompt_parts, send_commands_update, send_notice, send_update, submit_prompt, view_head,
};
use crate::jsonrpc::{ErrorObject, Id, Message};
use crate::transport::FrameWriter;

/// Dispatches one v2 request.
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
        "session/resume" => resume_session(host, state, writer, params)
            .await
            .map_or_else(fail, ok),
        "session/list" => list_shared(host, params, 50).await.map_or_else(fail, ok),
        "session/close" => close_shared(host, state, params)
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

/// Handles v2 `session/new`: opens, answers, then updates in order.
async fn new_session(
    host: &Host,
    state: &Arc<Mutex<AcpConn>>,
    writer: &FrameWriter,
    id: &Id,
    params: &Value,
) -> Option<Message> {
    let Some(cwd) = crate::rpc::opt_string(params, "cwd") else {
        return Some(Message::Error {
            id: id.clone(),
            error: crate::rpc::invalid_params("session/new", "missing member `cwd`"),
        });
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
    send_commands_update(host, AcpVersion::V2, writer, session).await;
    note_mcp_servers(writer, session, params).await;
    None
}

/// Handles v2 `session/resume`: binds, optionally replays from the start.
async fn resume_session(
    host: &Host,
    state: &Arc<Mutex<AcpConn>>,
    writer: &FrameWriter,
    params: &Value,
) -> Result<Value, ErrorObject> {
    let session = session_param(params)?;
    let agent = agent_for(host, state, session).await?;
    let replay = params
        .get("replayFrom")
        .and_then(|value| value.get("type"))
        .and_then(|value| value.as_str())
        == Some("start");
    if replay {
        super::replay::replay_history(&agent, session, writer).await?;
    }
    Ok(sonic_rs::json!({"sessionId": session.to_string()}))
}

/// Handles v2 `session/prompt`: accepts, echoes, then pumps to idle.
///
/// The handler owns the cancel token: cancellation submits a turn cancel and
/// the pump completes with the cancelled stop.
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
    let session = match session_param(params) {
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
    let (turn, message_id) = match submit_prompt(host, &agent, "session/prompt", parts).await {
        Ok(accepted) => accepted,
        Err(error) => return fail(error),
    };
    crate::rpc::send(
        writer,
        &Message::Result {
            id: id.clone(),
            result: sonic_rs::json!({"messageId": message_id}),
        },
    )
    .await;
    send_update(
        writer,
        session,
        sonic_rs::json!({
            "sessionUpdate": "user_message",
            "messageId": message_id,
            "content": prompt_echo(&blocks),
        }),
    )
    .await;
    let ctx = PumpCtx {
        state,
        writer,
        agent: &agent,
        session,
        version: AcpVersion::V2,
        prompt_turn: turn,
    };
    let end = prompt_pump(&ctx, subscription, cancel).await;
    finish_prompt(writer, session, end).await;
    None
}

/// Reports one prompt's end: its idle state, after a warning on failure.
async fn finish_prompt(writer: &FrameWriter, session: SessionId, end: PromptEnd) {
    let failure = match end {
        PromptEnd::Stopped(stop) => {
            let literal = stop_literal(stop);
            let Some(failed) = literal.get("failed") else {
                send_update(writer, session, state_update_idle(Some(literal))).await;
                return;
            };
            failed
                .get("message")
                .and_then(|message| message.as_str())
                .unwrap_or("the turn failed")
                .to_owned()
        }
        PromptEnd::Failed(text) => text,
    };
    send_notice(writer, session, "warning", failure).await;
    send_update(
        writer,
        session,
        state_update_idle(Some(Value::from("_dal_failed"))),
    )
    .await;
}

/// Echoes prompt blocks back as `user_message` content.
fn prompt_echo(blocks: &Value) -> Value {
    blocks.clone()
}

/// Reads the `sessionId` member shared by session methods.
fn session_param(params: &Value) -> Result<SessionId, ErrorObject> {
    let text = crate::rpc::opt_string(params, "sessionId").ok_or_else(|| {
        crate::rpc::invalid_params("session/prompt", "missing member `sessionId`")
    })?;
    SessionId::parse(&text)
        .map_err(|_| crate::rpc::invalid_params("session/prompt", "sessionId is not valid"))
}
