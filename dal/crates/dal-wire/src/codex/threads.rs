//! `thread/start`, `thread/resume`, and `thread/list`: threads are sessions.

use std::num::NonZeroU32;
use std::path::PathBuf;

use dal_agent::SessionRef;
use dal_core::{
    ApprovalMode, Block, EntryKind, ListQuery, ModelRoute, PageReq, Question, SessionId,
    SessionInfo, TurnState, View, Workspace,
};
use sonic_rs::{JsonContainerTrait, JsonValueMutTrait, JsonValueTrait, Value};

use super::items::{agent_message, assistant_text, reasoning, turn_value, user_message};
use super::{Ctx, Outcome, error_object};
use crate::jsonrpc::ErrorObject;
use crate::rpc::{agent_error, crate_version, host_error, invalid_params, provider_label};

/// Handles `thread/start`: replies, then emits `thread/started`.
pub(super) async fn start(ctx: &Ctx, id: &Value, params: &Value) -> Option<Outcome> {
    match open_new(ctx, params).await {
        Ok((response, thread)) => {
            ctx.reply(id, Ok(response)).await;
            ctx.notify("thread/started", sonic_rs::json!({"thread": thread}))
                .await;
            None
        }
        Err(error) => Some(Err(error)),
    }
}

/// Opens a new durable or ephemeral session; returns the response and thread.
async fn open_new(ctx: &Ctx, params: &Value) -> Result<(Value, Value), ErrorObject> {
    let method = "thread/start";
    let workspace = workspace(method, params.get("cwd"))?
        .ok_or_else(|| invalid_params(method, "cwd must be an absolute path"))?;
    let ephemeral = params
        .get("ephemeral")
        .and_then(sonic_rs::JsonValueTrait::as_bool)
        == Some(true);
    let session = if ephemeral {
        SessionRef::Ephemeral { workspace }
    } else {
        SessionRef::New {
            workspace,
            name: None,
        }
    };
    let agent = ctx
        .host
        .open(session, ctx.client().await?)
        .await
        .map_err(host_error)?;
    let view = agent
        .view(PageReq::default())
        .map_err(|error| agent_error(method, error))?;
    {
        let mut state = ctx.state.lock().await;
        state.threads.insert(view.session.id, agent);
        if ephemeral {
            state.ephemeral.insert(view.session.id);
        }
    }
    let thread = thread_value(&view.session, ephemeral, &live_status(&view), &[]);
    Ok((response(&view, &thread), thread))
}

/// Handles `thread/resume`: binds the session and returns its history.
pub(super) async fn resume(ctx: &Ctx, params: &Value) -> Outcome {
    let method = "thread/resume";
    let session = thread_id(method, params)?;
    let live = ctx.state.lock().await.threads.get(&session).cloned();
    let agent = if let Some(agent) = live {
        agent
    } else {
        let workspace = match workspace(method, params.get("cwd"))? {
            Some(workspace) => workspace,
            None => find_workspace(ctx, session)?,
        };
        let reference = SessionRef::Resume {
            key: session.to_string().into(),
            workspace,
        };
        let agent = ctx
            .host
            .open(reference, ctx.client().await?)
            .await
            .map_err(host_error)?;
        ctx.state
            .lock()
            .await
            .threads
            .insert(session, agent.clone());
        agent
    };
    let view = agent
        .view(PageReq::default())
        .map_err(|error| agent_error(method, error))?;
    let exclude = params
        .get("excludeTurns")
        .and_then(sonic_rs::JsonValueTrait::as_bool)
        == Some(true);
    let turns = if exclude { Vec::new() } else { history(&view) };
    let ephemeral = ctx.state.lock().await.ephemeral.contains(&session);
    let thread = thread_value(&view.session, ephemeral, &live_status(&view), &turns);
    Ok(response(&view, &thread))
}

/// Handles `thread/list`: pages sessions, filtered by workspace.
pub(super) async fn list(ctx: &Ctx, params: &Value) -> Outcome {
    let method = "thread/list";
    let limit = match params.get("limit").filter(|limit| !limit.is_null()) {
        None => None,
        Some(limit) => Some(
            limit
                .as_u64()
                .and_then(|limit| u32::try_from(limit).ok())
                .ok_or_else(|| invalid_params(method, "limit must be a u32"))?,
        ),
    };
    let cwd = cwd_filter(method, params.get("cwd"))?;
    let page = ctx
        .host
        .sessions(ListQuery {
            limit,
            cursor: params
                .get("cursor")
                .and_then(|value| value.as_str())
                .map(Box::from),
            search: params
                .get("searchTerm")
                .and_then(|value| value.as_str())
                .map(Box::from),
        })
        .map_err(host_error)?;
    let state = ctx.state.lock().await;
    let mut data = Vec::new();
    for info in &page.items {
        if !cwd.is_empty() && !cwd.iter().any(|path| path == info.workspace.as_path()) {
            continue;
        }
        let status = if let Some(agent) = state.threads.get(&info.id) {
            let page = PageReq::new(NonZeroU32::MIN, None)
                .map_err(|error| invalid_params(method, error.to_string()))?;
            let view = agent
                .view(page)
                .map_err(|error| agent_error(method, error))?;
            live_status(&view)
        } else {
            sonic_rs::json!({"type": "notLoaded"})
        };
        let ephemeral = state.ephemeral.contains(&info.id);
        data.push(thread_value(info, ephemeral, &status, &[]));
    }
    let mut result = sonic_rs::json!({"data": data});
    if let (Some(next), Some(object)) = (page.next_before, result.as_object_mut()) {
        object.insert("nextCursor", next.as_ref());
    }
    Ok(result)
}

/// Parses the required `threadId` member.
pub(super) fn thread_id(method: &str, params: &Value) -> Result<SessionId, ErrorObject> {
    params
        .get("threadId")
        .and_then(|value| value.as_str())
        .and_then(|id| SessionId::parse(id).ok())
        .ok_or_else(|| invalid_params(method, "threadId must be a thread id"))
}

/// Builds one Codex `Thread` value from a session row.
pub(crate) fn thread_value(
    info: &SessionInfo,
    ephemeral: bool,
    status: &Value,
    turns: &[Value],
) -> Value {
    let id = info.id.to_string();
    let created = info.created_at.unwrap_or(info.updated_at);
    let mut thread = sonic_rs::json!({
        "id": id.as_str(),
        "sessionId": id.as_str(),
        "cliVersion": crate_version(),
        "createdAt": created.as_second(),
        "updatedAt": info.updated_at.as_second(),
        "cwd": info.workspace.as_path().display().to_string(),
        "ephemeral": ephemeral,
        "modelProvider": "dal",
        "preview": info.preview.as_ref(),
        "projectId": null,
        "source": "appServer",
        "status": status,
        "turns": turns,
    });
    if let (Some(name), Some(object)) = (info.name.as_deref(), thread.as_object_mut()) {
        object.insert("name", name);
    }
    thread
}

/// Builds the shared `thread/start` and `thread/resume` response.
fn response(view: &View, thread: &Value) -> Value {
    let (model, provider) = match &view.settings.model {
        Some(route) => (model_label(route), provider_label(route)),
        None => ("default".to_owned(), "dal".to_owned()),
    };
    sonic_rs::json!({
        "thread": thread,
        "model": model,
        "modelProvider": provider,
        "cwd": view.session.workspace.as_path().display().to_string(),
        "approvalPolicy": approval_policy(view.settings.approval),
        "approvalsReviewer": "user",
        "sandbox": {"type": "dangerFullAccess"},
    })
}

/// Returns the display model of one route.
fn model_label(route: &ModelRoute) -> String {
    match route {
        ModelRoute::Api { model, .. } => model.to_string(),
        ModelRoute::Synthetic { id } | ModelRoute::Harness { id } => id.to_string(),
    }
}

/// Maps the dal approval mode to the Codex approval policy.
fn approval_policy(mode: ApprovalMode) -> &'static str {
    match mode {
        ApprovalMode::Ask => "untrusted",
        ApprovalMode::Edits => "on-request",
        ApprovalMode::All => "never",
    }
}

/// Maps a live view to the Codex thread status.
fn live_status(view: &View) -> Value {
    if matches!(view.turn, TurnState::Idle) {
        return sonic_rs::json!({"type": "idle"});
    }
    let mut flags = Vec::new();
    for request in &view.open {
        let flag = match request.question {
            Question::Approval { .. } | Question::Grant { .. } => "waitingOnApproval",
            _ => "waitingOnUserInput",
        };
        if !flags.contains(&flag) {
            flags.push(flag);
        }
    }
    sonic_rs::json!({"type": "active", "activeFlags": flags})
}

/// Groups the view's first entry page into turns: each user entry opens one.
fn history(view: &View) -> Vec<Value> {
    let mut turns = Vec::new();
    let mut current: Option<(String, Vec<Value>)> = None;
    for entry in &view.entries.items {
        let id = entry.id.to_string();
        match &entry.kind {
            EntryKind::User { parts } => {
                if let Some((turn, items)) = current.take() {
                    turns.push(turn_value(&turn, "completed", None, &items));
                }
                let item = user_message(&id, parts);
                current = Some((id, vec![item]));
            }
            EntryKind::Assistant { content, .. } => {
                let Some((_, items)) = current.as_mut() else {
                    continue;
                };
                for (index, block) in content.iter().enumerate() {
                    if let Block::Reasoning { text, .. } = block {
                        items.push(reasoning(&format!("{id}-{index}"), text));
                    }
                }
                let text = assistant_text(content);
                if !text.is_empty() {
                    items.push(agent_message(&id, &text));
                }
            }
            _ => {}
        }
    }
    if let Some((turn, items)) = current {
        let status = if matches!(view.turn, TurnState::Idle) {
            "completed"
        } else {
            "inProgress"
        };
        turns.push(turn_value(&turn, status, None, &items));
    }
    turns
}

/// Parses an optional absolute `cwd`.
fn workspace(method: &str, cwd: Option<&Value>) -> Result<Option<Workspace>, ErrorObject> {
    match cwd.filter(|cwd| !cwd.is_null()) {
        None => Ok(None),
        Some(cwd) => cwd
            .as_str()
            .and_then(|path| Workspace::new(PathBuf::from(path)).ok())
            .map(Some)
            .ok_or_else(|| invalid_params(method, "cwd must be an absolute path")),
    }
}

/// Parses the `thread/list` cwd filter: one path or a list of paths.
fn cwd_filter(method: &str, cwd: Option<&Value>) -> Result<Vec<PathBuf>, ErrorObject> {
    let Some(cwd) = cwd.filter(|cwd| !cwd.is_null()) else {
        return Ok(Vec::new());
    };
    if let Some(path) = cwd.as_str() {
        return Ok(vec![PathBuf::from(path)]);
    }
    cwd.as_array()
        .and_then(|paths| {
            paths
                .iter()
                .map(|path| path.as_str().map(PathBuf::from))
                .collect::<Option<Vec<_>>>()
        })
        .ok_or_else(|| invalid_params(method, "cwd must be a path or a list of paths"))
}

/// Finds one session's workspace through the host listing.
fn find_workspace(ctx: &Ctx, session: SessionId) -> Result<Workspace, ErrorObject> {
    let mut cursor = None;
    loop {
        let page = ctx
            .host
            .sessions(ListQuery {
                limit: None,
                cursor,
                search: None,
            })
            .map_err(host_error)?;
        if let Some(info) = page.items.into_iter().find(|info| info.id == session) {
            return Ok(info.workspace);
        }
        match page.next_before {
            Some(next) => cursor = Some(next),
            None => {
                return Err(error_object(
                    -32002,
                    format!("thread {session} was not found"),
                ));
            }
        }
    }
}
