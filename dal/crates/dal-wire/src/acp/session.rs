//! ACP session binding, prompt submission, cancellation, and transport-loss teardown.

use std::sync::Arc;

use dal_agent::{Agent, Host};
use dal_core::{Command, Expect, Part, SessionId, TurnId};
use sonic_rs::Value;
use tokio::sync::Mutex;

use super::{AcpConn, ServerAnswer, slash_command};
use crate::jsonrpc::{ErrorObject, Id};
use crate::transport::FrameWriter;

/// Cancels one session's running turn and its outstanding permission waits.
pub(super) async fn cancel_session(
    host: &Host,
    state: &Arc<Mutex<AcpConn>>,
    writer: &FrameWriter,
    params: &Value,
) {
    let Some(session) =
        crate::rpc::opt_string(params, "sessionId").and_then(|text| SessionId::parse(&text).ok())
    else {
        return;
    };
    let agent = state.lock().await.agents.get(&session).cloned();
    let Some(agent) = agent else {
        return;
    };
    let turn = agent
        .view(dal_core::PageReq::default())
        .ok()
        .and_then(|view| match view.turn {
            dal_core::TurnState::Running { turn } | dal_core::TurnState::Settling { turn } => {
                Some(turn)
            }
            dal_core::TurnState::Idle | dal_core::TurnState::Compacting { .. } => None,
        });
    if let Some(turn) = turn {
        let cancel = Command::Cancel {
            scope: dal_core::CancelScope::Turn(turn),
        };
        if agent.submit(cancel).await.is_err() {
            let _ = host;
        }
    }
    cancel_outstanding(state, writer, session).await;
}

/// Withdraws every outstanding permission of one session.
///
/// Removes each wait from the connection state, which ends its permission
/// flow with the core default, and sends `$/cancel_request` to the client.
pub(crate) async fn cancel_outstanding(
    state: &Arc<Mutex<AcpConn>>,
    writer: &FrameWriter,
    session: SessionId,
) {
    let ids: Vec<String> = {
        let mut locked = state.lock().await;
        let ids: Vec<String> = locked
            .pending
            .iter()
            .filter(|(_, (owner, _))| *owner == session)
            .map(|(client, _)| client.clone())
            .collect();
        for id in &ids {
            locked.pending.remove(id);
        }
        locked.outstanding.retain(|_, client| !ids.contains(client));
        ids
    };
    for id in ids {
        super::map::cancel_one(writer, &id).await;
    }
}

/// Completes one server-initiated request with the client's result.
pub(super) async fn complete_pending(state: &Arc<Mutex<AcpConn>>, id: &Id, answer: ServerAnswer) {
    let sender = state
        .lock()
        .await
        .pending
        .remove(&crate::rpc::id_key(id))
        .map(|(_, sender)| sender);
    if let Some(sender) = sender {
        let _ = sender.send(answer);
    } else {
        tracing::debug!(request = %crate::rpc::id_key(id), "late or unknown client response dropped");
    }
}

/// Drops one server-initiated request after a client error frame.
pub(super) async fn fail_pending(state: &Arc<Mutex<AcpConn>>, id: &Id) {
    state.lock().await.pending.remove(&crate::rpc::id_key(id));
}

/// Fails one server-initiated request named by a client `$/cancel_request`.
pub(super) async fn fail_pending_by_client(state: &Arc<Mutex<AcpConn>>, client: &str) {
    state.lock().await.pending.remove(client);
}

/// Cancels open turns and closes opened sessions after transport loss.
pub(super) async fn teardown(host: &Host, state: &Arc<Mutex<AcpConn>>) {
    let opened: Vec<SessionId> = state.lock().await.opened.iter().copied().collect();
    for session in opened {
        let agent = state.lock().await.agents.get(&session).cloned();
        if let Some(agent) = agent {
            if let Ok(view) = agent.view(dal_core::PageReq::default()) {
                let turn = match view.turn {
                    dal_core::TurnState::Running { turn }
                    | dal_core::TurnState::Settling { turn } => Some(turn),
                    dal_core::TurnState::Idle | dal_core::TurnState::Compacting { .. } => None,
                };
                if let Some(turn) = turn {
                    let cancel = Command::Cancel {
                        scope: dal_core::CancelScope::Turn(turn),
                    };
                    let _ = agent.submit(cancel).await;
                }
            }
            let _ = host.close(session).await;
        }
        let mut locked = state.lock().await;
        locked.agents.remove(&session);
        locked.opened.remove(&session);
    }
}

/// Opens (and records) one session for an absolute workspace.
pub(crate) async fn open_workspace(
    host: &Host,
    state: &Arc<Mutex<AcpConn>>,
    workspace: &str,
) -> Result<(SessionId, Agent), ErrorObject> {
    if !workspace.starts_with('/') {
        return Err(crate::rpc::invalid_params(
            "session/new",
            "cwd must be an absolute path",
        ));
    }
    let reference = dal_agent::SessionRef::New {
        workspace: workspace_to_core(workspace)?,
        name: None,
    };
    let agent = open_ref(host, state, &reference).await?;
    let session = agent_session(&agent)?;
    record_agent(state, session, agent.clone()).await;
    mark_opened(state, session).await;
    Ok((session, agent))
}

/// Opens one session reference through the host.
pub(crate) async fn open_ref(
    host: &Host,
    state: &Arc<Mutex<AcpConn>>,
    reference: &dal_agent::SessionRef,
) -> Result<Agent, ErrorObject> {
    let client = state.lock().await.client.clone();
    host.open(reference.clone(), client)
        .await
        .map_err(crate::rpc::host_error)
}

/// Records one bound session in the connection state.
pub(crate) async fn record_agent(state: &Arc<Mutex<AcpConn>>, session: SessionId, agent: Agent) {
    state.lock().await.agents.insert(session, agent);
}

/// Marks one session as opened by this connection for loss cleanup.
pub(crate) async fn mark_opened(state: &Arc<Mutex<AcpConn>>, session: SessionId) {
    state.lock().await.opened.insert(session);
}

/// Reads one bound agent's session id through its head view.
fn agent_session(agent: &Agent) -> Result<SessionId, ErrorObject> {
    agent
        .view(dal_core::PageReq::default())
        .map(|view| view.session.id)
        .map_err(|error| crate::rpc::agent_error("session/new", error))
}

/// Returns the connection's hold for one session id.
pub(crate) async fn agent_for(
    host: &Host,
    state: &Arc<Mutex<AcpConn>>,
    id: SessionId,
) -> Result<Agent, ErrorObject> {
    if let Some(agent) = state.lock().await.agents.get(&id) {
        return Ok(agent.clone());
    }
    let workspace = crate::rpc::session::find_workspace(host, id)?;
    let reference = dal_agent::SessionRef::Resume {
        key: id.to_string().into(),
        workspace,
    };
    let agent = open_ref(host, state, &reference).await?;
    record_agent(state, id, agent.clone()).await;
    Ok(agent)
}

/// Converts an absolute workspace string to the core value.
fn workspace_to_core(workspace: &str) -> Result<dal_core::Workspace, ErrorObject> {
    dal_core::Workspace::try_from(std::path::PathBuf::from(workspace))
        .map_err(|_| crate::rpc::invalid_params("session/new", "cwd must be an absolute path"))
}

/// Views one session head.
pub(crate) async fn view_head(agent: &Agent, method: &str) -> Result<dal_core::View, ErrorObject> {
    agent
        .view(dal_core::PageReq::default())
        .map_err(|error| crate::rpc::agent_error(method, error))
}

/// Rejects a prompt while any turn runs, including wake turns.
pub(crate) fn check_idle(view: &dal_core::View) -> Result<(), ErrorObject> {
    match &view.turn {
        dal_core::TurnState::Idle => Ok(()),
        dal_core::TurnState::Running { turn } | dal_core::TurnState::Settling { turn } => {
            Err(ErrorObject {
                code: -32004,
                message: format!("expected an idle session, actual turn {turn} running"),
                data: None,
            })
        }
        dal_core::TurnState::Compacting { job } => Err(ErrorObject {
            code: -32004,
            message: format!("expected an idle session, actual turn {job} running"),
            data: None,
        }),
    }
}

/// Rejects a prompt with no model or no provider credentials; a session with
/// no model of its own uses the host's configured default route.
///
/// `E1` text is owned by the CLI part; the message below is interim until
/// that literal arrives.
pub(crate) async fn check_model(host: &Host, view: &dal_core::View) -> Result<(), ErrorObject> {
    let route = match view.settings.model.clone() {
        Some(route) => Some(route),
        None => host.default_route().await,
    };
    let Some(route) = route else {
        return Err(ErrorObject {
            code: -32001,
            message: "no model is configured for this session".to_owned(),
            data: Some(sonic_rs::json!({
                "hint": "set a model with /model or configure a default model",
            })),
        });
    };
    if let Some(provider) = crate::rpc::missing_credential(host, &route) {
        return Err(ErrorObject {
            code: -32000,
            message: format!("{provider} has no credentials: auth.json has no entry for it"),
            data: Some(sonic_rs::json!({
                "hint": format!("Run dalgon login {provider} or set {}.", crate::rpc::env_name(&provider)),
            })),
        });
    }
    Ok(())
}

/// Submits one prompt (or slash command) and returns the accepted turn.
pub(crate) async fn submit_prompt(
    host: &Host,
    agent: &Agent,
    method: &str,
    parts: Vec<Part>,
) -> Result<(TurnId, String), ErrorObject> {
    if let Some((name, args)) = slash_command(&parts) {
        let known = host
            .commands()
            .iter()
            .any(|spec| spec.name.as_str() == name);
        if known {
            let command = Command::Run {
                name: name.into(),
                args: args.into(),
                expected: None,
            };
            return submit_accepted(agent, method, command).await;
        }
    }
    let command = Command::Prompt {
        expect: Expect::Idle,
        content: parts,
    };
    submit_accepted(agent, method, command).await
}

/// Submits one command and extracts the accepted turn and message id.
async fn submit_accepted(
    agent: &Agent,
    method: &str,
    command: Command,
) -> Result<(TurnId, String), ErrorObject> {
    match agent
        .submit(command)
        .await
        .map_err(|error| crate::rpc::agent_error(method, error))?
    {
        dal_core::Reply::Accepted { turn, message_id } => Ok((turn, message_id.to_string())),
        _ => Err(ErrorObject {
            code: -32603,
            message: "internal error: the prompt did not start a turn".to_owned(),
            data: Some(crate::rpc::hint_value()),
        }),
    }
}
