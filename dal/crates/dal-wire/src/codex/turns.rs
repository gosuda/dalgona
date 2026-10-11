//! `turn/start`, `turn/interrupt`, and the per-turn update pump.

use std::collections::HashMap;
use std::num::NonZeroU64;
use std::path::PathBuf;

use dal_agent::{Agent, Delivery, Subscription};
use dal_core::{
    AssistantStop, CancelScope, Command, EntryKind, Expect, PageReq, Part, Reply, Request,
    RequestId, SessionId, TurnId, TurnState,
};
use futures::future::BoxFuture;
use futures::{FutureExt, StreamExt, stream::FuturesUnordered};
use sonic_rs::{JsonContainerTrait, JsonValueTrait, Value};
use tokio_util::sync::CancellationToken;

use super::ask::{client_answer, server_request};
use super::items::{Event, TurnStream, turn_value};
use super::threads::thread_id;
use super::{Ctx, Outcome, error_object};
use crate::jsonrpc::ErrorObject;
use crate::rpc::{agent_error, invalid_params};

/// How a client wait for one server request ended.
enum ClientReply {
    /// The client returned a result.
    Result(Value),
    /// The client returned an error.
    Error,
    /// No reply arrived: the wait was cancelled, timed out, or dropped.
    Missing,
}

/// A client wait for one server request.
type Wait = BoxFuture<'static, (Request, ClientReply)>;

/// Handles `turn/start`: subscribes, submits, replies, then pumps the turn.
pub(super) async fn start(ctx: &Ctx, id: &Value, params: &Value) -> Option<Outcome> {
    let method = "turn/start";
    let (session, agent, cwd, content) = match prepare(ctx, params).await {
        Ok(prepared) => prepared,
        Err(error) => return Some(Err(error)),
    };
    let subscription = match agent.subscribe(None) {
        Ok(subscription) => subscription,
        Err(error) => return Some(Err(agent_error(method, error))),
    };
    let prompt = Command::Prompt {
        expect: Expect::Idle,
        content,
    };
    let turn = match agent.submit(prompt).await {
        Ok(Reply::Accepted { turn, .. }) => turn,
        Ok(_) => {
            let message = "internal error: the prompt did not start a turn".to_owned();
            return Some(Err(error_object(-32603, message)));
        }
        Err(error) => return Some(Err(agent_error(method, error))),
    };
    let thread = session.to_string();
    let started = turn_value(&turn.to_string(), "inProgress", None, &[]);
    ctx.reply(id, Ok(sonic_rs::json!({"turn": &started}))).await;
    ctx.notify(
        "turn/started",
        sonic_rs::json!({"threadId": thread.as_str(), "turn": started}),
    )
    .await;
    let pump = Pump {
        ctx,
        agent: &agent,
        stream: TurnStream::new(thread, turn),
        cwd,
        turn,
        waits: FuturesUnordered::new(),
        outstanding: HashMap::new(),
    };
    pump.run(subscription).await;
    None
}

/// Handles `turn/interrupt`: cancels the named running turn.
pub(super) async fn interrupt(ctx: &Ctx, params: &Value) -> Outcome {
    let method = "turn/interrupt";
    let session = thread_id(method, params)?;
    let turn = params
        .get("turnId")
        .and_then(|value| value.as_str())
        .and_then(|text| text.parse::<u64>().ok())
        .and_then(NonZeroU64::new)
        .map(TurnId::new)
        .ok_or_else(|| invalid_params(method, "turnId must be a turn id"))?;
    let agent = ctx.thread(method, session).await?;
    agent
        .submit(Command::Cancel {
            scope: CancelScope::Turn(turn),
        })
        .await
        .map_err(|error| agent_error(method, error))?;
    Ok(sonic_rs::json!({}))
}

/// Resolves the thread, its agent and workspace, and the prompt content.
async fn prepare(
    ctx: &Ctx,
    params: &Value,
) -> Result<(SessionId, Agent, PathBuf, Vec<Part>), ErrorObject> {
    let method = "turn/start";
    let session = thread_id(method, params)?;
    let agent = ctx.thread(method, session).await?;
    let content = content(method, params)?;
    let view = agent
        .view(PageReq::default())
        .map_err(|error| agent_error(method, error))?;
    Ok((
        session,
        agent,
        view.session.workspace.into_path_buf(),
        content,
    ))
}

/// Maps Codex `UserInput` items to prompt parts; only text is accepted.
fn content(method: &str, params: &Value) -> Result<Vec<Part>, ErrorObject> {
    let input = params
        .get("input")
        .and_then(|value| value.as_array())
        .ok_or_else(|| invalid_params(method, "input must be an array"))?;
    let mut parts = Vec::with_capacity(input.len());
    for item in input {
        match item.get("type").and_then(|value| value.as_str()) {
            Some("text") => {
                let text = item
                    .get("text")
                    .and_then(|value| value.as_str())
                    .ok_or_else(|| invalid_params(method, "text input needs text"))?;
                parts.push(Part::Text { text: text.into() });
            }
            Some(other) => {
                let detail = format!("input type \"{other}\" is not supported");
                return Err(invalid_params(method, detail));
            }
            None => return Err(invalid_params(method, "input item needs a type")),
        }
    }
    if parts.is_empty() {
        return Err(invalid_params(method, "input is empty"));
    }
    Ok(parts)
}

/// One running turn pump: maps updates and brokers approval round trips.
struct Pump<'a> {
    ctx: &'a Ctx,
    agent: &'a Agent,
    stream: TurnStream,
    cwd: PathBuf,
    turn: TurnId,
    waits: FuturesUnordered<Wait>,
    outstanding: HashMap<RequestId, (i64, CancellationToken)>,
}

impl Pump<'_> {
    /// Runs until the turn completes, the session closes, or the connection ends.
    async fn run(mut self, mut subscription: Subscription) {
        while !self.stream.is_done() {
            tokio::select! {
                biased;
                () = self.ctx.stop.cancelled() => break,
                Some((request, result)) = self.waits.next(), if !self.waits.is_empty() => {
                    self.settle(&request, result).await;
                }
                delivery = subscription.next() => match delivery {
                    None => break,
                    Some(Delivery::Update(update)) => {
                        for event in self.stream.apply(&update.kind) {
                            self.emit(event).await;
                        }
                    }
                    Some(Delivery::Resync { .. }) => match self.agent.subscribe(None) {
                        Ok(fresh) => {
                            subscription = fresh;
                            self.recover().await;
                        }
                        Err(error) => {
                            tracing::debug!(%error, "codex turn resubscribe failed");
                            break;
                        }
                    },
                },
            }
        }
        for (server, token) in self.outstanding.into_values() {
            token.cancel();
            self.ctx.forget(server).await;
        }
    }

    /// Writes one mapped event.
    async fn emit(&mut self, event: Event) {
        match event {
            Event::Notify(method, params) => self.ctx.notify(method, params).await,
            Event::Ask(request) => self.ask(*request).await,
            Event::Resolved(id) => self.drop_wait(id).await,
        }
    }

    /// Ends the client wait for one resolved request.
    async fn drop_wait(&mut self, id: RequestId) {
        if let Some((server, token)) = self.outstanding.remove(&id) {
            token.cancel();
            self.ctx.forget(server).await;
        }
    }

    /// Sends the server request for one core request and waits in the set.
    /// Does nothing when the request is already outstanding.
    async fn ask(&mut self, request: Request) {
        if self.outstanding.contains_key(&request.id) {
            return;
        }
        let Some((method, params)) =
            server_request(self.stream.thread(), self.turn, &self.cwd, &request)
        else {
            return;
        };
        let (server, receiver) = self.ctx.request(method, params).await;
        let token = CancellationToken::new();
        self.outstanding.insert(request.id, (server, token.clone()));
        let timeout = request.timeout;
        self.waits.push(
            async move {
                let result = tokio::select! {
                    biased;
                    () = token.cancelled() => ClientReply::Missing,
                    result = receiver => match result {
                        Ok(Some(value)) => ClientReply::Result(value),
                        Ok(None) => ClientReply::Error,
                        Err(_) => ClientReply::Missing,
                    },
                    () = tokio::time::sleep(timeout) => ClientReply::Missing,
                };
                (request, result)
            }
            .boxed(),
        );
    }

    /// Answers the core with the client result; a missing result leaves the default.
    async fn settle(&mut self, request: &Request, reply: ClientReply) {
        if let Some((server, _)) = self.outstanding.remove(&request.id) {
            self.ctx.forget(server).await;
        }
        let answer = match reply {
            ClientReply::Result(value) => client_answer(request, Some(&value)),
            ClientReply::Error => client_answer(request, None),
            ClientReply::Missing => return,
        };
        if let Err(error) = self.agent.answer(request.id, answer).await {
            tracing::debug!(%error, request = %request.id, "codex answer was not applied");
        }
    }

    /// Repairs the pump after lost updates: re-asks open requests of a running
    /// turn, or ends the turn from the view when it already stopped.
    async fn recover(&mut self) {
        let Ok(view) = self.agent.view(PageReq::default()) else {
            return;
        };
        let running = match view.turn {
            TurnState::Running { turn } | TurnState::Settling { turn } => turn == self.turn,
            _ => false,
        };
        if running {
            let open: Vec<RequestId> = view.open.iter().map(|request| request.id).collect();
            let stale: Vec<RequestId> = self
                .outstanding
                .keys()
                .filter(|id| !open.contains(id))
                .copied()
                .collect();
            for id in stale {
                self.drop_wait(id).await;
            }
            for request in view.open {
                if request.turn.is_none_or(|turn| turn == self.turn) {
                    self.ask(request).await;
                }
            }
            return;
        }
        let stop = view
            .entries
            .items
            .iter()
            .rev()
            .find_map(|entry| match &entry.kind {
                EntryKind::Assistant { stop, .. } => Some(stop),
                _ => None,
            });
        let (status, error) = match stop {
            Some(AssistantStop::Cancelled) => ("interrupted", None),
            Some(AssistantStop::Failed { message }) => ("failed", Some(message.as_ref())),
            _ => ("completed", None),
        };
        for event in self.stream.finish(status, error) {
            self.emit(event).await;
        }
    }
}
