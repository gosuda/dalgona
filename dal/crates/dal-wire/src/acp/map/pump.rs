//! ACP prompt pump: live updates to notifications until the prompt turn ends.
//!
//! Permission and elicitation waits run as futures owned by the pump, beside
//! the update stream, so a later `RequestResolved` or a cancel is handled
//! while a client request is outstanding. Ending the pump drops every open
//! wait and cancels its client request.

use std::sync::Arc;

use dal_agent::{Agent, Delivery, Subscription};
use dal_core::{Command, Question, SessionId, Stop, TurnId, Update, UpdateKind};
use futures::future::BoxFuture;
use futures::stream::FuturesUnordered;
use futures::{FutureExt, StreamExt};
use sonic_rs::JsonValueTrait;
use tokio::sync::Mutex;
use tokio_util::sync::CancellationToken;

use super::super::{AcpConn, AcpVersion, cancel_outstanding, send_notice, send_update};
use super::request::PermissionEnd;
use super::{cancel_unaskable, map_update, may_elicit, permission_flow, resolved_elsewhere};
use crate::transport::FrameWriter;

/// The open permission waits of one prompt pump.
pub(crate) type Flows = FuturesUnordered<BoxFuture<'static, PermissionEnd>>;

/// The connection-side context one prompt pump works in.
pub(crate) struct PumpCtx<'a> {
    /// The shared connection state.
    pub state: &'a Arc<Mutex<AcpConn>>,
    /// The connection writer.
    pub writer: &'a FrameWriter,
    /// The bound session handle.
    pub agent: &'a Agent,
    /// The prompted session.
    pub session: SessionId,
    /// The connection's branch.
    pub version: AcpVersion,
    /// The turn the prompt started.
    pub prompt_turn: TurnId,
}

/// How one opened request is routed to the client.
enum Route {
    /// Ask the client through a permission or elicitation request.
    Ask,
    /// The branch cannot ask it: resolve `Cancel` at once.
    Refuse,
    /// An unknown question: the core default resolves it.
    Default,
}

/// Handles one prompt-turn update inside a prompt pump.
///
/// Opened requests start a wait in `flows` instead of blocking the pump.
/// Returns the terminal stop when the prompt's own turn ends.
pub(crate) async fn pump_update(
    ctx: &PumpCtx<'_>,
    flows: &mut Flows,
    update: &Update,
) -> Option<Stop> {
    match &update.kind {
        UpdateKind::RequestOpened(request) => {
            match route(ctx.state, ctx.version, &request.question).await {
                Route::Ask => flows.push(
                    permission_flow(
                        Arc::clone(ctx.state),
                        ctx.writer.clone(),
                        ctx.agent.clone(),
                        ctx.session,
                        ctx.version,
                        request.clone(),
                    )
                    .boxed(),
                ),
                Route::Refuse => {
                    cancel_unaskable(ctx.writer, ctx.agent, ctx.session, request).await;
                }
                Route::Default => {}
            }
            None
        }
        UpdateKind::RequestResolved { id, .. } => {
            resolved_elsewhere(ctx.state, ctx.writer, *id).await;
            None
        }
        UpdateKind::TurnEnded { turn, stop } if *turn == ctx.prompt_turn => Some(*stop),
        _ => {
            for body in map_update(ctx.version, ctx.session, update, Some(ctx.prompt_turn)) {
                if body.get("sessionUpdate").is_some() || body.get("_dal/notice").is_some() {
                    send_update(ctx.writer, ctx.session, body).await;
                }
            }
            None
        }
    }
}

/// Routes one question to the client, the Cancel path, or the core default.
async fn route(state: &Arc<Mutex<AcpConn>>, version: AcpVersion, question: &Question) -> Route {
    let asked = match question {
        Question::Approval { .. } | Question::Grant { .. } | Question::Confirm { .. } => true,
        Question::Select { multi, .. } => !multi || may_elicit(state, version).await,
        Question::Text { .. } => may_elicit(state, version).await,
        _ => return Route::Default,
    };
    if asked { Route::Ask } else { Route::Refuse }
}

/// A source of prompt-turn deliveries: a live subscription in production.
pub(crate) trait Deliveries: Sized + Send {
    /// Returns the next delivery, or `None` when the source closed.
    fn next(&mut self) -> impl Future<Output = Option<Delivery>> + Send;

    /// Resumes after a resync at `position` while the prompt turn still runs.
    fn resume(
        &self,
        agent: &Agent,
        position: (dal_core::Gen, dal_core::Seq),
        prompt_turn: TurnId,
    ) -> Option<Self>;
}

impl Deliveries for Subscription {
    fn next(&mut self) -> impl Future<Output = Option<Delivery>> + Send {
        Subscription::next(self)
    }

    fn resume(
        &self,
        agent: &Agent,
        position: (dal_core::Gen, dal_core::Seq),
        prompt_turn: TurnId,
    ) -> Option<Self> {
        resume_after_resync(agent, position, prompt_turn)
    }
}

/// Runs one prompt pump until the prompt turn ends or the wait is cancelled.
///
/// `source` must be registered before the prompt was submitted so no
/// update of the prompt turn is missed. Open permission waits end with the
/// pump; their client requests receive `$/cancel_request`.
pub(crate) async fn prompt_pump<D: Deliveries>(
    ctx: &PumpCtx<'_>,
    source: D,
    cancel: &CancellationToken,
) -> PromptEnd {
    let end = pump_loop(ctx, source, cancel).await;
    cancel_outstanding(ctx.state, ctx.writer, ctx.session).await;
    end
}

/// The pump's select loop; owns and drops the open permission waits.
async fn pump_loop<D: Deliveries>(
    ctx: &PumpCtx<'_>,
    mut source: D,
    cancel: &CancellationToken,
) -> PromptEnd {
    let mut flows = Flows::new();
    let mut cancelled_once = false;
    loop {
        tokio::select! {
            biased;
            () = cancel.cancelled(), if !cancelled_once => {
                cancelled_once = true;
                cancel_outstanding(ctx.state, ctx.writer, ctx.session).await;
                let stop = Command::Cancel { scope: dal_core::CancelScope::Turn(ctx.prompt_turn) };
                let _ = ctx.agent.submit(stop).await;
            }
            _ = flows.next(), if !flows.is_empty() => {}
            delivery = source.next() => match delivery {
                None => return PromptEnd::Failed("the session closed during the prompt".to_owned()),
                Some(Delivery::Resync { generation, seq }) => {
                    send_notice(ctx.writer, ctx.session, "warning", SLOW_TEXT.to_owned()).await;
                    match source.resume(ctx.agent, (generation, seq), ctx.prompt_turn) {
                        Some(resumed) => source = resumed,
                        None => return PromptEnd::Failed(SLOW_TEXT.to_owned()),
                    }
                }
                Some(Delivery::Update(update)) => {
                    if let Some(stop) = pump_update(ctx, &mut flows, &update).await {
                        return PromptEnd::Stopped(stop);
                    }
                }
            },
        }
    }
}

/// The notice and failure text for a subscription that fell behind.
const SLOW_TEXT: &str = "dal dropped updates because the client read too slowly";

/// Resubscribes at the resync position while the prompt turn still runs.
fn resume_after_resync(
    agent: &Agent,
    position: (dal_core::Gen, dal_core::Seq),
    prompt_turn: TurnId,
) -> Option<Subscription> {
    let resumed = agent.subscribe(Some(position)).ok()?;
    let head = agent.view(dal_core::PageReq::default()).ok()?;
    match head.turn {
        dal_core::TurnState::Running { turn } | dal_core::TurnState::Settling { turn }
            if turn == prompt_turn =>
        {
            Some(resumed)
        }
        _ => None,
    }
}

/// How one prompt pump finished.
pub(crate) enum PromptEnd {
    /// The prompt turn ended with this stop.
    Stopped(Stop),
    /// The pump failed before the turn ended.
    Failed(String),
}
