//! The per-task follower driven by the accept loop, and task waits.
//!
//! Each task has one follower that owns its subscription (registered before
//! the prompt was submitted), folds updates into the task table, fans stream
//! responses out to SSE sinks, and closes them after the final status.
//! Stream disconnects never cancel the turn; listener shutdown sends one
//! cancel.

use std::sync::Arc;
use std::time::Duration;

use dal_agent::{Agent, Delivery, Subscription};
use dal_core::{CancelScope, Command, PageReq, TurnState};
use tokio::sync::{mpsc, watch};

use super::events::{Step, apply, frame, step, task_json, task_response};
use super::table::{Sink, TaskKey};
use crate::a2a::{TaskEvent, TaskState};
use crate::serve::ServeCtx;

/// The SSE sink capacity.
pub(crate) const SINK_CAPACITY: usize = 256;
/// Slots kept free for the final artifact, the final status, and the close.
const FINAL_FRAMES: usize = 3;

/// Starts the follower for one registered task.
pub(crate) async fn start(
    ctx: &Arc<ServeCtx>,
    agent: Agent,
    subscription: Subscription,
    key: TaskKey,
) {
    let owned = Arc::clone(ctx);
    ctx.drive(Box::pin(follow(owned, agent, subscription, key)))
        .await;
}

/// Follows one task until its terminal status.
async fn follow(ctx: Arc<ServeCtx>, agent: Agent, mut subscription: Subscription, key: TaskKey) {
    let mut cancel_sent = false;
    loop {
        let delivery = tokio::select! {
            biased;
            () = ctx.shutdown.cancelled(), if !cancel_sent => {
                cancel_sent = true;
                let cancel = Command::Cancel { scope: CancelScope::Turn(key.turn) };
                if let Err(error) = agent.submit(cancel).await {
                    tracing::debug!(%error, "a2a cancel on shutdown failed");
                }
                continue;
            }
            delivery = subscription.next() => delivery,
        };
        let update = match delivery {
            Some(Delivery::Update(update)) => update,
            Some(Delivery::Resync { .. }) => {
                let Some(next) = resubscribe(&agent, key) else {
                    publish(&ctx, key, Step::End(TaskEvent::Fail)).await;
                    return;
                };
                subscription = next;
                continue;
            }
            None => {
                publish(&ctx, key, Step::End(TaskEvent::Fail)).await;
                return;
            }
        };
        let pending = ctx
            .a2a
            .lock()
            .await
            .task(key)
            .and_then(|task| task.request.as_ref().map(|request| request.id));
        if let Some(next) = step(key.turn, pending, &update.kind)
            && publish(&ctx, key, next).await
        {
            return;
        }
    }
}

/// Resubscribes live after a resync while the turn still runs.
fn resubscribe(agent: &Agent, key: TaskKey) -> Option<Subscription> {
    let subscription = agent.subscribe(None).ok()?;
    let view = agent.view(PageReq::default()).ok()?;
    match view.turn {
        TurnState::Running { turn } | TurnState::Settling { turn } if turn == key.turn => {
            Some(subscription)
        }
        _ => None,
    }
}

/// Folds one step into the table and sends its events; returns true at the end.
///
/// Sends happen under the table lock so they stay ordered with [`attach`].
/// Each sink gets all of a step's frames or none: a sink without room for
/// them plus the two final frames and the close is dropped whole, which
/// ends its stream. The turn keeps running.
async fn publish(ctx: &Arc<ServeCtx>, key: TaskKey, next: Step) -> bool {
    let ending = matches!(next, Step::End(_));
    let mut table = ctx.a2a.lock().await;
    let Some(task) = table.task_mut(key) else {
        return true;
    };
    if task.state.is_terminal() {
        return true;
    }
    let events = apply(task, next);
    let reserve = if ending { 1 } else { FINAL_FRAMES };
    task.sinks.retain(|sink| {
        if sink.tx.capacity() < events.len() + reserve {
            return false;
        }
        events.iter().all(|event| {
            sink.tx
                .try_send(Some(frame(&sink.framing, event.clone())))
                .is_ok()
        })
    });
    if ending {
        for sink in std::mem::take(&mut task.sinks) {
            let _ = sink.tx.try_send(None);
        }
    }
    ending
}

/// Attaches one SSE sink, sending the current task first.
///
/// Returns `None` when the task is not retained.
pub(crate) async fn attach(
    ctx: &Arc<ServeCtx>,
    key: TaskKey,
    framing: super::table::Framing,
) -> Option<mpsc::Receiver<Option<String>>> {
    let (tx, rx) = mpsc::channel(SINK_CAPACITY);
    let mut table = ctx.a2a.lock().await;
    let task = table.task_mut(key)?;
    let first = frame(&framing, task_response(task));
    if tx.try_send(Some(first)).is_err() {
        return None;
    }
    if task.state.is_terminal() {
        let _ = tx.try_send(None);
    } else {
        task.sinks.push(Sink { tx, framing });
    }
    Some(rx)
}

/// Returns a state receiver for one task with its current value marked seen.
pub(crate) async fn watch_task(
    ctx: &Arc<ServeCtx>,
    key: TaskKey,
) -> Option<watch::Receiver<TaskState>> {
    let table = ctx.a2a.lock().await;
    let mut receiver = table.task(key)?.watch.subscribe();
    receiver.mark_unchanged();
    Some(receiver)
}

/// Returns true for states a blocking send stops at.
const fn settled(state: TaskState) -> bool {
    state.is_terminal() || matches!(state, TaskState::InputRequired)
}

/// Waits until the task is terminal or needs input, starting with its current state.
pub(crate) async fn wait_settled(ctx: &Arc<ServeCtx>, mut receiver: watch::Receiver<TaskState>) {
    if settled(*receiver.borrow()) {
        return;
    }
    wait_changed(ctx, &mut receiver, settled).await;
}

/// Waits for a state change that satisfies `done`, ignoring the current state.
pub(crate) async fn wait_next(ctx: &Arc<ServeCtx>, receiver: &mut watch::Receiver<TaskState>) {
    wait_changed(ctx, receiver, settled).await;
}

/// Waits at most `limit` for a terminal state.
pub(crate) async fn wait_terminal(
    ctx: &Arc<ServeCtx>,
    receiver: &mut watch::Receiver<TaskState>,
    limit: Duration,
) {
    if receiver.borrow().is_terminal() {
        return;
    }
    let _ = tokio::time::timeout(limit, wait_changed(ctx, receiver, TaskState::is_terminal)).await;
}

/// Waits for changes until `done` holds, the task is dropped, or shutdown.
async fn wait_changed(
    ctx: &Arc<ServeCtx>,
    receiver: &mut watch::Receiver<TaskState>,
    done: fn(TaskState) -> bool,
) {
    loop {
        tokio::select! {
            biased;
            () = ctx.shutdown.cancelled() => return,
            changed = receiver.changed() => {
                if changed.is_err() || done(*receiver.borrow_and_update()) {
                    return;
                }
            }
        }
    }
}

/// Renders the current task view, when retained.
pub(crate) async fn view(ctx: &Arc<ServeCtx>, key: TaskKey) -> Option<sonic_rs::Value> {
    ctx.a2a.lock().await.task(key).map(task_json)
}
