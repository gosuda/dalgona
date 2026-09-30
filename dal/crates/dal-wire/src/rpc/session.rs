//! `session/*` method handlers for version-1 RPC.
//!
//! Method envelopes follow architecture §4.3; values are the shared
//! `dal-core` serde types, never wire-specific copies. Every handler that
//! touches a session binds (or reuses) the connection's [`Agent`] hold so a
//! disconnect releases holds without cancelling turns.

use std::num::{NonZeroU32, NonZeroU64};
use std::sync::Arc;

use dal_agent::{Agent, Host, SessionRef};
use dal_core::{Command, EntryId, Gen, ListQuery, PageReq, RequestId, Seq, SessionId};
use sonic_rs::{JsonValueMutTrait, JsonValueTrait, Value};
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

use super::{
    Conn, agent_error, decode_params, host_error, invalid_params, opt_i64, opt_string, send,
    to_value,
};
use crate::jsonrpc::{ErrorObject, Id, Message};
use crate::transport::FrameWriter;

/// Maximum `session/list` page size.
const LIST_MAX: i64 = 500;
/// Maximum `session/view` page size.
const VIEW_MAX: u32 = 200;
/// Largest single view page in bytes of JSON.
const VIEW_BYTE_CAP: usize = 1_048_576;

/// Handles `session/list`: pages session rows across known workspaces.
pub(crate) fn list(host: &Host, params: &Value) -> Result<Value, ErrorObject> {
    let limit = match opt_i64(params, "limit") {
        None => 50,
        Some(number) if (1..=LIST_MAX).contains(&number) => {
            u32::try_from(number).map_err(|_| {
                invalid_params(
                    "session/list",
                    format!("limit must be between 1 and {LIST_MAX}"),
                )
            })?
        }
        Some(_) => {
            return Err(invalid_params(
                "session/list",
                format!("limit must be between 1 and {LIST_MAX}"),
            ));
        }
    };
    let query = ListQuery {
        limit: Some(limit),
        cursor: opt_string(params, "cursor").map(String::into_boxed_str),
        search: opt_string(params, "search").map(String::into_boxed_str),
    };
    let page = host.sessions(query).map_err(host_error)?;
    let items = to_value(&page.items)?;
    let mut result = sonic_rs::json!({"sessions": items});
    if let Some(cursor) = page.next_before
        && let Some(object) = result.as_object_mut()
    {
        object.insert("nextCursor", Value::from(cursor.as_ref()));
    }
    Ok(result)
}

/// Handles `session/open`: opens one session and returns its head view.
pub(crate) async fn open(
    host: &Host,
    state: &Arc<Mutex<Conn>>,
    params: &Value,
) -> Result<Value, ErrorObject> {
    if let Some(workspace) = params
        .get("ref")
        .and_then(|reference| reference.get("workspace"))
        && let Some(path) = workspace.as_str()
        && !path.starts_with('/')
    {
        return Err(invalid_params(
            "session/open",
            "workspace must be an absolute path",
        ));
    }
    let reference: SessionRef = params
        .get("ref")
        .map(|reference| decode_params::<SessionRef>("session/open", reference))
        .transpose()?
        .ok_or_else(|| invalid_params("session/open", "missing member `ref`"))?;
    if matches!(reference, SessionRef::Child { .. }) {
        return Err(invalid_params(
            "session/open",
            "child sessions cannot be opened from the wire",
        ));
    }
    let agent = open_agent(host, state, &reference).await?;
    let view = agent
        .view(PageReq::default())
        .map_err(|error| agent_error("session/open", error))?;
    let id = view.session.id;
    state.lock().await.agents.insert(id, agent.clone());
    let r#gen = view.r#gen.get();
    let view = to_value(&view)?;
    Ok(sonic_rs::json!({
        "sessionId": id.to_string(),
        "gen": r#gen,
        "view": view,
    }))
}

/// Handles `session/close`: flushes one session and drops the local hold.
pub(crate) async fn close(
    host: &Host,
    state: &Arc<Mutex<Conn>>,
    params: &Value,
) -> Result<Value, ErrorObject> {
    let id = session_param("session/close", params)?;
    host.close(id).await.map_err(host_error)?;
    let mut locked = state.lock().await;
    locked.agents.remove(&id);
    if let Some((_, token)) = locked.subs.remove(&id) {
        token.cancel();
    }
    Ok(sonic_rs::json!({}))
}

/// Handles `session/view`: returns one bounded page of the head view.
pub(crate) async fn view(
    host: &Host,
    state: &Arc<Mutex<Conn>>,
    params: &Value,
) -> Result<Value, ErrorObject> {
    let id = session_param("session/view", params)?;
    let limit = match opt_i64(params, "limit") {
        None => 50,
        Some(number)
            if number >= 1
                && u64::try_from(number).is_ok_and(|counter| counter <= u64::from(VIEW_MAX)) =>
        {
            u32::try_from(number).map_err(|_| {
                invalid_params(
                    "session/view",
                    format!("limit must be between 1 and {VIEW_MAX}"),
                )
            })?
        }
        Some(_) => {
            return Err(invalid_params(
                "session/view",
                format!("limit must be between 1 and {VIEW_MAX}"),
            ));
        }
    };
    let before = match opt_string(params, "before") {
        None => None,
        Some(text) => {
            let counter: u64 = text
                .parse()
                .map_err(|_| invalid_params("session/view", "before is not a valid entry id"))?;
            Some(EntryId::new(NonZeroU64::new(counter).ok_or_else(|| {
                invalid_params("session/view", "before is not a valid entry id")
            })?))
        }
    };
    let agent = agent_for(host, state, id).await?;
    let mut size = limit;
    loop {
        let limit = NonZeroU32::new(size).ok_or_else(|| ErrorObject {
            code: -32603,
            message: "internal error: page limit is zero".to_owned(),
            data: Some(super::hint_value()),
        })?;
        let page = PageReq::new(limit, before).map_err(|_| {
            invalid_params(
                "session/view",
                format!("limit must be between 1 and {VIEW_MAX}"),
            )
        })?;
        let snapshot = agent
            .view(page)
            .map_err(|error| agent_error("session/view", error))?;
        let value = to_value(&snapshot)?;
        let text = sonic_rs::to_string(&value).map_err(|error| ErrorObject {
            code: -32603,
            message: format!("internal error: {error}"),
            data: Some(super::hint_value()),
        })?;
        if text.len() <= VIEW_BYTE_CAP || size <= 1 {
            return Ok(value);
        }
        size /= 2;
    }
}

/// Handles `session/subscribe`: replies `{r#gen,seq}`, then pumps updates.
///
/// The reply is written inline before any notification; this handler returns
/// `None` so the dispatcher sends no second reply.
pub(crate) async fn subscribe(
    host: &Host,
    state: &Arc<Mutex<Conn>>,
    writer: &FrameWriter,
    id: &Id,
    params: &Value,
) -> Option<Message> {
    let reply = |result: Value| Message::Result {
        id: id.clone(),
        result,
    };
    let fail = |error: ErrorObject| Message::Error {
        id: id.clone(),
        error,
    };
    let session = match session_param("session/subscribe", params) {
        Ok(session) => session,
        Err(error) => return Some(fail(error)),
    };
    let agent = match agent_for(host, state, session).await {
        Ok(agent) => agent,
        Err(error) => return Some(fail(error)),
    };
    let head = match agent.view(PageReq::default()) {
        Ok(view) => view,
        Err(error) => return Some(fail(agent_error("session/subscribe", error))),
    };
    let (r#gen, head_seq) = (head.r#gen, head.seq);
    let want_gen = match opt_i64(params, "gen") {
        None => r#gen,
        Some(number) => {
            let counter = u64::try_from(number).ok().and_then(NonZeroU64::new);
            match counter {
                Some(counter) => Gen::new(counter),
                None => {
                    return Some(fail(invalid_params(
                        "session/subscribe",
                        "gen is not valid",
                    )));
                }
            }
        }
    };
    let after = match opt_i64(params, "after") {
        None => None,
        Some(number) => {
            let counter = u64::try_from(number).ok().and_then(NonZeroU64::new);
            match counter {
                Some(counter) => Some(Seq::new(counter)),
                None => {
                    return Some(fail(invalid_params(
                        "session/subscribe",
                        "after is not valid",
                    )));
                }
            }
        }
    };
    if want_gen != r#gen {
        return emit_resync(state, writer, id, session, agent, r#gen, head_seq).await;
    }
    if let Some(after) = after
        && after.get() > head_seq.get()
    {
        return Some(fail(invalid_params(
            "session/subscribe",
            format!("after {} is beyond seq {}", after.get(), head_seq.get()),
        )));
    }
    let cursor = after.map(|after| (r#gen, after));
    let subscription = match agent.subscribe(cursor) {
        Ok(subscription) => subscription,
        Err(error) => return Some(fail(agent_error("session/subscribe", error))),
    };
    send(
        writer,
        &reply(sonic_rs::json!({"gen": r#gen.get(), "seq": head_seq.get()})),
    )
    .await;
    let fence = replace_sub(state, session).await;
    super::session_pump(state.clone(), writer.clone(), session, fence, subscription).await;
    None
}

/// Emits an immediate resync for a stale generation and resumes at the head.
async fn emit_resync(
    state: &Arc<Mutex<Conn>>,
    writer: &FrameWriter,
    id: &Id,
    session: SessionId,
    agent: Agent,
    r#gen: Gen,
    seq: dal_core::Seq,
) -> Option<Message> {
    let subscription = match agent.subscribe(Some((r#gen, seq))) {
        Ok(subscription) => subscription,
        Err(error) => {
            return Some(Message::Error {
                id: id.clone(),
                error: agent_error("session/subscribe", error),
            });
        }
    };
    send(
        writer,
        &Message::Result {
            id: id.clone(),
            result: sonic_rs::json!({"gen": r#gen.get(), "seq": seq.get()}),
        },
    )
    .await;
    super::send_resync(writer, session, r#gen, seq).await;
    let fence = replace_sub(state, session).await;
    super::session_pump(state.clone(), writer.clone(), session, fence, subscription).await;
    None
}

/// Handles `session/unsubscribe`: atomically replaces the session pump.
pub(crate) async fn unsubscribe(
    state: &Arc<Mutex<Conn>>,
    params: &Value,
) -> Result<Value, ErrorObject> {
    let id = session_param("session/unsubscribe", params)?;
    let mut locked = state.lock().await;
    if let Some((_, token)) = locked.subs.remove(&id) {
        token.cancel();
    }
    Ok(sonic_rs::json!({}))
}

/// Handles `session/submit`: submits one strict command and returns its reply.
pub(crate) async fn submit(
    host: &Host,
    state: &Arc<Mutex<Conn>>,
    params: &Value,
) -> Result<Value, ErrorObject> {
    let id = session_param("session/submit", params)?;
    let raw = params
        .get("command")
        .ok_or_else(|| invalid_params("session/submit", "missing member `command`"))?;
    if let Some(tag) = raw.get("type").and_then(|value| value.as_str())
        && !is_command(tag)
    {
        return Err(invalid_params(
            "session/submit",
            format!(r#"unknown command type "{tag}""#),
        ));
    }
    let command: Command = decode_params("session/submit", raw)?;
    let agent = agent_for(host, state, id).await?;
    let reply = agent
        .submit(command)
        .await
        .map_err(|error| agent_error("session/submit", error))?;
    to_value(&reply)
}

/// Handles `session/answer`: answers one open request.
pub(crate) async fn answer(
    host: &Host,
    state: &Arc<Mutex<Conn>>,
    params: &Value,
) -> Result<Value, ErrorObject> {
    let id = session_param("session/answer", params)?;
    let request = opt_string(params, "requestId")
        .ok_or_else(|| invalid_params("session/answer", "missing member `requestId`"))?;
    let request = RequestId::parse(&request)
        .map_err(|_| invalid_params("session/answer", "requestId is not valid"))?;
    let raw = params
        .get("answer")
        .ok_or_else(|| invalid_params("session/answer", "missing member `answer`"))?;
    if let Some(tag) = raw.get("type").and_then(|value| value.as_str())
        && !is_answer(tag)
    {
        return Err(invalid_params(
            "session/answer",
            format!(r#"unknown answer type "{tag}""#),
        ));
    }
    let answer: dal_core::Answer = decode_params("session/answer", raw)?;
    let agent = agent_for(host, state, id).await?;
    agent
        .answer(request, answer)
        .await
        .map_err(|error| agent_error("session/answer", error))?;
    Ok(sonic_rs::json!({}))
}

/// Reads the `sessionId` member shared by session methods.
pub(crate) fn session_param(method: &str, params: &Value) -> Result<SessionId, ErrorObject> {
    let text = opt_string(params, "sessionId")
        .ok_or_else(|| invalid_params(method, "missing member `sessionId`"))?;
    SessionId::parse(&text).map_err(|_| invalid_params(method, "sessionId is not valid"))
}

/// Opens one session through the host and caches the connection hold.
async fn open_agent(
    host: &Host,
    state: &Arc<Mutex<Conn>>,
    reference: &SessionRef,
) -> Result<Agent, ErrorObject> {
    let locked = state.lock().await;
    let client = locked.client.clone();
    drop(locked);
    let agent = host
        .open(reference.clone(), client)
        .await
        .map_err(host_error)?;
    Ok(agent)
}

/// Returns the connection's hold for one session, binding on first use.
pub(crate) async fn agent_for(
    host: &Host,
    state: &Arc<Mutex<Conn>>,
    id: SessionId,
) -> Result<Agent, ErrorObject> {
    if let Some(agent) = state.lock().await.agents.get(&id) {
        return Ok(agent.clone());
    }
    let workspace = find_workspace(host, id)?;
    let reference = SessionRef::Resume {
        key: id.to_string().into(),
        workspace,
    };
    let agent = open_agent(host, state, &reference).await?;
    Ok(state.lock().await.agents.entry(id).or_insert(agent).clone())
}

/// Finds one session's workspace through the host listing.
pub(crate) fn find_workspace(
    host: &Host,
    id: SessionId,
) -> Result<dal_core::Workspace, ErrorObject> {
    let query = ListQuery {
        limit: None,
        cursor: None,
        search: None,
    };
    let page = host.sessions(query).map_err(host_error)?;
    page.items
        .into_iter()
        .find(|info| info.id == id)
        .map(|info| info.workspace)
        .ok_or_else(|| ErrorObject {
            code: -32002,
            message: format!("session {id} was not found"),
            data: None,
        })
}

/// Replaces one session subscription; returns the new fence value.
async fn replace_sub(state: &Arc<Mutex<Conn>>, id: SessionId) -> u64 {
    let mut locked = state.lock().await;
    locked.fence += 1;
    let fence = locked.fence;
    if let Some((_, token)) = locked.subs.insert(id, (fence, CancellationToken::new())) {
        token.cancel();
    }
    fence
}

/// Known command discriminator values.
fn is_command(tag: &str) -> bool {
    matches!(
        tag,
        "prompt"
            | "steer"
            | "follow_up"
            | "cancel"
            | "set_model"
            | "set_thinking"
            | "set_approval"
            | "compact"
            | "move_leaf"
            | "fork"
            | "clone"
            | "rename"
            | "run"
    )
}

/// Known answer discriminator values.
fn is_answer(tag: &str) -> bool {
    matches!(
        tag,
        "approve" | "approve_for_session" | "decline" | "cancel" | "value"
    )
}
