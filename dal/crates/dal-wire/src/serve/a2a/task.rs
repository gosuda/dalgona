//! `SendMessage`, `SendStreamingMessage`, `GetTask`, and `CancelTask`.
//!
//! Context is a session and task is a turn. An absent context opens a
//! session at the captured serve workspace; one context never runs two
//! turns at once.

use std::sync::Arc;
use std::time::Duration;

use dal_agent::{Agent, AgentError};
use dal_core::{CancelScope, Command, Expect, PageReq, Part, Reply, Save, SessionId, TurnState};
use sonic_rs::{JsonValueTrait, Value};
use tokio::sync::mpsc;

use super::error::Fail;
use super::events::{failed_task_json, frame};
use super::follow;
use super::parts::{answer_for, prompt_parts, prompt_text};
use super::table::{Framing, TaskKey, TaskRec};
use crate::a2a::TaskState;
use crate::router::HarnessMode;
use crate::router::decode::{ResolvedModel, resolve_model};
use crate::serve::{ServeCtx, router_options};

/// Cancel-task wait for the terminal update.
const CANCEL_WAIT: Duration = Duration::from_secs(1);

/// The result of one A2A method.
pub(crate) enum Outcome {
    /// A complete JSON result.
    Value(Value),
    /// A live SSE stream.
    Stream(mpsc::Receiver<Option<String>>),
}

/// How the caller wants one send answered.
#[derive(Clone, Debug)]
pub(crate) enum Respond {
    /// Wait until terminal or input-required.
    Blocking,
    /// Return the task at once.
    Immediate,
    /// Stream every event with this framing.
    Stream(Framing),
}

/// The model target named by `metadata.model`.
enum Target {
    /// Run the dal harness in this mode.
    Mode(HarnessMode),
    /// Use this model route.
    Route(dal_core::ModelRoute),
}

/// Handles `SendMessage` and `SendStreamingMessage`.
///
/// # Errors
///
/// Returns the mapped A2A failure; nothing is mutated on failure.
pub(crate) async fn send_message(
    ctx: &Arc<ServeCtx>,
    params: &Value,
    streaming: Option<Framing>,
) -> Result<Outcome, Fail> {
    let message = params
        .get("message")
        .filter(sonic_rs::JsonValueTrait::is_object)
        .ok_or_else(|| Fail::invalid("params.message is required"))?;
    let reply = match streaming {
        Some(framing) => Respond::Stream(framing),
        None if params
            .pointer(["configuration", "returnImmediately"])
            .and_then(JsonValueTrait::as_bool)
            .unwrap_or(false) =>
        {
            Respond::Immediate
        }
        None => Respond::Blocking,
    };
    let target = match params
        .pointer(["metadata", "model"])
        .and_then(JsonValueTrait::as_str)
    {
        Some(model) => Some(resolve_target(ctx, model)?),
        None => None,
    };
    let raw_parts = message.get("parts").cloned().unwrap_or_default();
    let parts = prompt_parts(&raw_parts)?;
    let context = message.get("contextId").and_then(JsonValueTrait::as_str);
    if let Some(task_id) = message.get("taskId").and_then(JsonValueTrait::as_str) {
        let key = TaskKey::parse(task_id).ok_or_else(|| Fail::task_not_found(task_id))?;
        if context.is_some_and(|context| context != key.session.to_string()) {
            return Err(Fail::invalid(format!(
                "contextId does not match task {task_id}"
            )));
        }
        return continue_task(ctx, key, &raw_parts, reply).await;
    }
    let (session, agent, fresh) = resolve_context(ctx, context).await?;
    let live = ctx.a2a.lock().await.live_task(session).map(|task| task.key);
    if let Some(key) = live {
        return continue_task(ctx, key, &raw_parts, reply).await;
    }
    match start_task(ctx, (session, agent), fresh, target, parts).await? {
        Started::Task(key) => finish(ctx, key, reply).await,
        Started::Failed(task) => Ok(failed_outcome(task, &reply)),
    }
}

/// Continues a named task: answers its question or reports its state.
async fn continue_task(
    ctx: &Arc<ServeCtx>,
    key: TaskKey,
    parts: &Value,
    reply: Respond,
) -> Result<Outcome, Fail> {
    let (state, request) = {
        let table = ctx.a2a.lock().await;
        let task = table
            .task(key)
            .ok_or_else(|| Fail::task_not_found(&key.to_string()))?;
        (task.state, task.request.clone())
    };
    if state.is_terminal() {
        return Err(Fail::invalid(format!(
            "task {key} is {}: send a new message in the same contextId",
            state.wire_name()
        )));
    }
    let Some(request) = request.filter(|_| state == TaskState::InputRequired) else {
        return Err(conflict(key));
    };
    let answer = answer_for(&request.question, parts)?;
    let agent = context_agent(ctx, key.session).await?;
    let mut receiver = follow::watch_task(ctx, key)
        .await
        .ok_or_else(|| Fail::task_not_found(&key.to_string()))?;
    let stream = match &reply {
        Respond::Stream(framing) => Some(attach(ctx, key, framing.clone()).await?),
        _ => None,
    };
    agent
        .answer(request.id, answer)
        .await
        .map_err(|error| Fail::invalid(error.to_string()))?;
    if let Some(stream) = stream {
        return Ok(Outcome::Stream(stream));
    }
    if matches!(reply, Respond::Blocking) {
        follow::wait_next(ctx, &mut receiver).await;
    }
    task_outcome(ctx, key).await
}

/// The result of starting one prompt turn.
enum Started {
    /// The turn runs as this task.
    Task(TaskKey),
    /// The prompt submit failed; this is the rendered FAILED task.
    Failed(Value),
}

/// Opens, binds, and prepares one prompt turn.
///
/// A prompt submit that fails without a turn yields a FAILED task that is
/// not retained, because no turn id exists to name it.
async fn start_task(
    ctx: &Arc<ServeCtx>,
    bound: (SessionId, Agent),
    fresh: bool,
    target: Option<Target>,
    parts: Vec<Part>,
) -> Result<Started, Fail> {
    let (session, agent) = bound;
    let head = agent
        .view(PageReq::default())
        .map_err(|error| Fail::internal(error.to_string()))?;
    busy_check(&head.turn, session)?;
    ctx.a2a.lock().await.reserve()?;
    if let Some(target) = target.filter(|target| fresh || matches!(target, Target::Route(_))) {
        apply_target(&agent, target).await?;
    }
    if fresh {
        let mode = crate::router::harness::strictest(head.settings.approval, ctx.cfg.approval);
        submit(
            &agent,
            Command::SetApproval {
                mode,
                save: Save::SessionOnly,
            },
        )
        .await?;
    }
    let prompt = prompt_text(&parts);
    let subscription = agent
        .subscribe(None)
        .map_err(|error| Fail::internal(error.to_string()))?;
    let command = Command::Prompt {
        expect: Expect::Idle,
        content: parts,
    };
    let turn = match agent.submit(command).await {
        Ok(Reply::Accepted { turn, .. }) => turn,
        Ok(_) => {
            let text = "the prompt did not start a turn";
            return Ok(Started::Failed(failed_task_json(session, &prompt, text)));
        }
        Err(error @ AgentError::WrongTurn { .. }) => return Err(busy(&agent, session, &error)),
        Err(error) => {
            let text = error.to_string();
            return Ok(Started::Failed(failed_task_json(session, &prompt, &text)));
        }
    };
    let key = TaskKey { session, turn };
    let inserted = ctx.a2a.lock().await.insert(TaskRec::started(key, prompt));
    if let Err(fail) = inserted {
        let _ = agent
            .submit(Command::Cancel {
                scope: CancelScope::Turn(turn),
            })
            .await;
        return Err(fail);
    }
    follow::start(ctx, agent, subscription, key).await;
    Ok(Started::Task(key))
}

/// Answers a failed submit: the FAILED task, or a stream holding only it.
fn failed_outcome(task: Value, reply: &Respond) -> Outcome {
    let mut response = sonic_rs::Object::new();
    response.insert("task", task);
    let response = Value::from(response);
    let Respond::Stream(framing) = reply else {
        return Outcome::Value(response);
    };
    let (tx, rx) = mpsc::channel(2);
    let _ = tx.try_send(Some(frame(framing, response)));
    let _ = tx.try_send(None);
    Outcome::Stream(rx)
}

/// Answers a started task as the caller asked.
async fn finish(ctx: &Arc<ServeCtx>, key: TaskKey, reply: Respond) -> Result<Outcome, Fail> {
    match reply {
        Respond::Stream(framing) => attach(ctx, key, framing).await.map(Outcome::Stream),
        Respond::Immediate => task_outcome(ctx, key).await,
        Respond::Blocking => {
            if let Some(receiver) = follow::watch_task(ctx, key).await {
                follow::wait_settled(ctx, receiver).await;
            }
            task_outcome(ctx, key).await
        }
    }
}

/// Attaches an SSE sink or reports the vanished task.
async fn attach(
    ctx: &Arc<ServeCtx>,
    key: TaskKey,
    framing: Framing,
) -> Result<mpsc::Receiver<Option<String>>, Fail> {
    follow::attach(ctx, key, framing)
        .await
        .ok_or_else(|| Fail::task_not_found(&key.to_string()))
}

/// Wraps the current task in a `SendMessageResponse`.
async fn task_outcome(ctx: &Arc<ServeCtx>, key: TaskKey) -> Result<Outcome, Fail> {
    let task = follow::view(ctx, key)
        .await
        .ok_or_else(|| Fail::task_not_found(&key.to_string()))?;
    Ok(Outcome::Value(sonic_rs::json!({"task": task})))
}

/// Builds the busy-context conflict.
fn conflict(key: TaskKey) -> Fail {
    Fail::invalid(format!(
        "context {} is running task {key}: wait for it, or cancel it",
        key.session
    ))
}

/// Refuses a prompt while the context runs any turn.
fn busy_check(turn: &TurnState, session: SessionId) -> Result<(), Fail> {
    match turn {
        TurnState::Running { turn } | TurnState::Settling { turn } => Err(conflict(TaskKey {
            session,
            turn: *turn,
        })),
        _ => Ok(()),
    }
}

/// Maps a lost idle race to the busy conflict of the turn that won it.
fn busy(agent: &Agent, session: SessionId, error: &AgentError) -> Fail {
    agent
        .view(PageReq::default())
        .ok()
        .and_then(|view| busy_check(&view.turn, session).err())
        .unwrap_or_else(|| Fail::internal(error.to_string()))
}

/// Resolves `metadata.model` to a harness mode or a model route.
fn resolve_target(ctx: &Arc<ServeCtx>, model: &str) -> Result<Target, Fail> {
    match resolve_model(&router_options(&ctx.cfg), model) {
        Ok(ResolvedModel::Harness(mode)) => Ok(Target::Mode(mode)),
        Ok(ResolvedModel::Route(route)) => Ok(Target::Route(route)),
        Err(fail) => Err(Fail::invalid(fail.message)),
    }
}

/// Selects the resolved mode or route on the context session.
async fn apply_target(agent: &Agent, target: Target) -> Result<(), Fail> {
    let command = match target {
        Target::Mode(mode) => Command::Run {
            name: "mode".into(),
            args: mode.mode_arg().into(),
            expected: None,
        },
        Target::Route(model) => Command::SetModel {
            model,
            save: Save::SessionOnly,
        },
    };
    submit(agent, command).await
}

/// Submits one setup command.
async fn submit(agent: &Agent, command: Command) -> Result<(), Fail> {
    agent
        .submit(command)
        .await
        .map(drop)
        .map_err(|error| Fail::internal(error.to_string()))
}

/// Resolves one context, opening a session at the serve workspace when absent.
///
/// Returns the session, its bound agent, and whether it is new.
async fn resolve_context(
    ctx: &Arc<ServeCtx>,
    context: Option<&str>,
) -> Result<(SessionId, Agent, bool), Fail> {
    if let Some(context) = context {
        let unknown = || Fail::invalid(format!("unknown contextId \"{context}\""));
        let session = SessionId::parse(context).map_err(|_| unknown())?;
        let agent = ctx.a2a.lock().await.agent(session).ok_or_else(unknown)?;
        return Ok((session, agent, false));
    }
    let workspace = dal_core::Workspace::try_from(ctx.cfg.workspace.clone())
        .map_err(|error| Fail::internal(error.to_string()))?;
    let agent = ctx
        .host
        .open(
            dal_agent::SessionRef::New {
                workspace,
                name: None,
            },
            crate::protocol::mint_client_id("a2a"),
        )
        .await
        .map_err(|error| Fail::internal(error.to_string()))?;
    let session = agent
        .view(PageReq::default())
        .map_err(|error| Fail::internal(error.to_string()))?
        .session
        .id;
    let evicted = {
        let mut table = ctx.a2a.lock().await;
        let evicted = table.remember_context(session);
        table.bind_agent(session, agent.clone());
        evicted
    };
    for old in evicted {
        if let Err(error) = ctx.host.close(old).await {
            tracing::debug!(%error, "closing an evicted a2a context failed");
        }
    }
    Ok((session, agent, true))
}

/// Returns the agent bound to a retained context.
async fn context_agent(ctx: &Arc<ServeCtx>, session: SessionId) -> Result<Agent, Fail> {
    ctx.a2a
        .lock()
        .await
        .agent(session)
        .ok_or_else(|| Fail::invalid(format!("unknown contextId \"{session}\"")))
}

/// Reads the `id` member naming one task.
fn task_key(params: &Value) -> Result<TaskKey, Fail> {
    let id = params
        .get("id")
        .and_then(JsonValueTrait::as_str)
        .ok_or_else(|| Fail::invalid("params.id is required"))?;
    TaskKey::parse(id).ok_or_else(|| Fail::task_not_found(id))
}

/// Handles `GetTask`.
///
/// # Errors
///
/// Returns `-32001` for unknown tasks.
pub(crate) async fn get_task(ctx: &Arc<ServeCtx>, params: &Value) -> Result<Outcome, Fail> {
    let key = task_key(params)?;
    follow::view(ctx, key)
        .await
        .map(Outcome::Value)
        .ok_or_else(|| Fail::task_not_found(&key.to_string()))
}

/// Handles `CancelTask`: cancels an active task and waits at most one second.
///
/// # Errors
///
/// Returns `-32001` for unknown tasks and `-32002` for terminal ones.
pub(crate) async fn cancel_task(ctx: &Arc<ServeCtx>, params: &Value) -> Result<Outcome, Fail> {
    let key = task_key(params)?;
    let state = ctx
        .a2a
        .lock()
        .await
        .task(key)
        .map(|task| task.state)
        .ok_or_else(|| Fail::task_not_found(&key.to_string()))?;
    if state.is_terminal() {
        return Err(Fail::new(
            "TASK_NOT_CANCELABLE",
            format!("task {key} is {}", state.wire_name()),
        ));
    }
    let agent = context_agent(ctx, key.session).await?;
    let mut receiver = follow::watch_task(ctx, key)
        .await
        .ok_or_else(|| Fail::task_not_found(&key.to_string()))?;
    let cancel = Command::Cancel {
        scope: CancelScope::Turn(key.turn),
    };
    if let Err(error) = agent.submit(cancel).await {
        tracing::debug!(%error, "a2a cancel was not accepted");
    }
    follow::wait_terminal(ctx, &mut receiver, CANCEL_WAIT).await;
    follow::view(ctx, key)
        .await
        .map(Outcome::Value)
        .ok_or_else(|| Fail::task_not_found(&key.to_string()))
}
