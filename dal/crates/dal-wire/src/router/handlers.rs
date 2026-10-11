//! HTTP route handlers for the four OpenAI-compatible routes.
//!
//! Each handler decodes its family body, resolves the model, and either runs
//! the harness or relays to the provider. Streaming replies, harness or
//! relay, run in a driver owned by the accept loop and send each SSE line as
//! its event arrives.

use std::sync::Arc;

use sonic_rs::Value;
use tokio::sync::mpsc;

use super::decode::{
    ResolvedModel, RouterFail, decode_chat, decode_messages, decode_responses, listed_ids,
    resolve_model,
};
use super::harness::{
    HarnessEvent, HarnessRequest, HarnessShared, HarnessStop, HarnessTurn, HttpParts, run_request,
};
use super::pass::RelayTurn;
use super::sink::{Discard, EventSink, SseEncoder, StreamSink};
use super::stream::{self, StreamIds, TurnIds};
use crate::router::HarnessMode;
use crate::serve::{Resp, ServeCtx};

/// Live stream channel capacity; a full channel blocks the turn observer.
const STREAM_CAPACITY: usize = 256;

/// Handles `GET /v1/models`: harness ids first, then aliases, then catalog.
pub(crate) async fn models(ctx: &Arc<ServeCtx>, anthropic: bool) -> Resp {
    let catalog = match ctx.host.models(None).await {
        Ok(catalog) => catalog,
        Err(error) => {
            tracing::warn!(%error, "router model catalog is unavailable; listing no catalog ids");
            Vec::new()
        }
    };
    let ids = listed_ids(&ctx.cfg.aliases, &catalog);
    if anthropic {
        let data: Vec<Value> = ids
            .iter()
            .map(|id| sonic_rs::json!({"id": id, "type": "model", "display_name": id}))
            .collect();
        Resp::json(200, &sonic_rs::json!({"data": data}))
    } else {
        let data: Vec<Value> = ids
            .iter()
            .map(|id| sonic_rs::json!({"id": id, "object": "model", "created": 0, "owned_by": "dal"}))
            .collect();
        Resp::json(200, &sonic_rs::json!({"object": "list", "data": data}))
    }
}

/// Handles `POST /v1/chat/completions`.
pub(crate) async fn chat(ctx: &Arc<ServeCtx>, body: &[u8], http: HttpParts) -> Resp {
    let decoded = match parse_body(body)
        .and_then(|value| decode_chat(&value).map_err(|fail| fail_response(&fail, None)))
    {
        Ok(decoded) => decoded,
        Err(resp) => return resp,
    };
    let model = decoded.request.model.clone();
    let encoder = decoded
        .request
        .stream
        .then(|| stream::ChatEncoder::new(&model, decoded.request.include_usage));
    let reply = Reply {
        model,
        http,
        ignored: ignored_header(&decoded.ignored),
        harness: stream::chat_object,
        relay: stream::relay_chat_object,
    };
    reply
        .run(ctx, HarnessRequest::Chat(decoded.request), encoder)
        .await
}

/// Handles `POST /v1/responses`.
pub(crate) async fn responses(ctx: &Arc<ServeCtx>, body: &[u8], http: HttpParts) -> Resp {
    let decoded = match parse_body(body)
        .and_then(|value| decode_responses(&value).map_err(|fail| fail_response(&fail, None)))
    {
        Ok(decoded) => decoded,
        Err(resp) => return resp,
    };
    let model = decoded.request.model.clone();
    let encoder = decoded
        .request
        .stream
        .then(|| stream::ResponsesEncoder::new(&model));
    let reply = Reply {
        model,
        http,
        ignored: ignored_header(&decoded.ignored),
        harness: stream::responses_object,
        relay: stream::relay_responses_object,
    };
    reply
        .run(ctx, HarnessRequest::Responses(decoded.request), encoder)
        .await
}

/// Handles `POST /v1/messages`.
pub(crate) async fn messages(ctx: &Arc<ServeCtx>, body: &[u8], http: HttpParts) -> Resp {
    let decoded = match parse_body(body)
        .and_then(|value| decode_messages(&value).map_err(|fail| fail_response(&fail, None)))
    {
        Ok(decoded) => decoded,
        Err(resp) => return resp,
    };
    let model = decoded.request.model.clone();
    let encoder = decoded
        .request
        .stream
        .then(|| stream::MessagesEncoder::new(&model));
    let reply = Reply {
        model,
        http,
        ignored: ignored_header(&decoded.ignored),
        harness: stream::message_object,
        relay: stream::relay_message_object,
    };
    reply
        .run(ctx, HarnessRequest::Messages(decoded.request), encoder)
        .await
}

/// One router reply: a live SSE stream or a finished family object.
struct Reply {
    model: String,
    http: HttpParts,
    ignored: Option<String>,
    harness: fn(&StreamIds, &HarnessTurn) -> Value,
    relay: fn(&StreamIds, &RelayTurn) -> Value,
}

impl Reply {
    /// Resolves the model and runs the harness or the relay.
    async fn run<E: SseEncoder + 'static>(
        self,
        ctx: &Arc<ServeCtx>,
        request: HarnessRequest,
        stream: Option<E>,
    ) -> Resp {
        match resolve_model(&crate::serve::router_options(&ctx.cfg), &self.model) {
            Ok(ResolvedModel::Harness(mode)) => self.harness(ctx, mode, request, stream).await,
            Ok(ResolvedModel::Route(route)) => self.relay(ctx, &route, request, stream).await,
            Err(fail) => fail_response(&fail, self.ignored),
        }
    }

    /// Runs one request through the harness.
    async fn harness<E: SseEncoder + 'static>(
        self,
        ctx: &Arc<ServeCtx>,
        mode: HarnessMode,
        request: HarnessRequest,
        stream: Option<E>,
    ) -> Resp {
        let shared = harness_shared(ctx);
        let Some(encoder) = stream else {
            return match run_request(&shared, mode, &request, &self.http, &mut Discard).await {
                Ok(HarnessTurn {
                    stop: HarnessStop::Failed(message),
                    ..
                }) => fail_response(&RouterFail::failed(message), self.ignored),
                Ok(HarnessTurn {
                    stop: HarnessStop::Cancelled,
                    ..
                }) => fail_response(
                    &RouterFail::failed("the turn was cancelled".to_owned()),
                    self.ignored,
                ),
                Ok(turn) => {
                    let ids = stream::stream_ids(&self.model, turn.session, turn.turn);
                    with_ignored(Resp::json(200, &(self.harness)(&ids, &turn)), self.ignored)
                }
                Err(fail) => fail_response(&fail, self.ignored),
            };
        };
        let (tx, rx) = mpsc::channel(STREAM_CAPACITY);
        let http = self.http;
        ctx.drive(Box::pin(async move {
            let mut sink = StreamSink::new(encoder, tx);
            if let Err(fail) = run_request(&shared, mode, &request, &http, &mut sink).await {
                sink.emit(HarnessEvent::Stop(HarnessStop::Failed(fail.message)))
                    .await;
            }
        }))
        .await;
        with_ignored(Resp::stream(rx, "text/event-stream"), self.ignored)
    }

    /// Relays one request to its provider route.
    async fn relay<E: SseEncoder + 'static>(
        self,
        ctx: &Arc<ServeCtx>,
        route: &dal_core::ModelRoute,
        request: HarnessRequest,
        stream: Option<E>,
    ) -> Resp {
        let shared = harness_shared(ctx);
        let relay = match super::pass::open(&shared, route, &request).await {
            Ok(relay) => relay,
            Err(fail) => return fail_response(&fail, self.ignored),
        };
        let ids = TurnIds::relay();
        let Some(encoder) = stream else {
            return match relay.collect(&shared.shutdown).await {
                Ok(turn) => {
                    let ids = StreamIds {
                        turn: ids,
                        model: self.model,
                    };
                    with_ignored(Resp::json(200, &(self.relay)(&ids, &turn)), self.ignored)
                }
                Err(fail) => fail_response(&fail, self.ignored),
            };
        };
        let (tx, rx) = mpsc::channel(STREAM_CAPACITY);
        let shutdown = shared.shutdown;
        ctx.drive(Box::pin(async move {
            let mut sink = StreamSink::new(encoder, tx);
            relay.pump(ids, &shutdown, &mut sink).await;
        }))
        .await;
        with_ignored(Resp::stream(rx, "text/event-stream"), self.ignored)
    }
}

/// Parses one request body as JSON.
fn parse_body(body: &[u8]) -> Result<Value, Resp> {
    let text = std::str::from_utf8(body).map_err(|_| {
        Resp::router_error(
            400,
            "invalid_request",
            "request body is not valid JSON: invalid UTF-8",
        )
    })?;
    sonic_rs::from_str(text).map_err(|error| {
        Resp::router_error(
            400,
            "invalid_request",
            &format!("request body is not valid JSON: {error}"),
        )
    })
}

/// Converts a router failure into its response with an optional ignored header.
fn fail_response(fail: &RouterFail, ignored: Option<String>) -> Resp {
    let mut resp = Resp::router_error(fail.status, fail.code, &fail.message);
    if let Some(header) = ignored {
        resp.headers.push(("x-dal-ignored".to_owned(), header));
    }
    resp
}

/// Attaches the ignored-members header to a success response.
fn with_ignored(mut resp: Resp, ignored: Option<String>) -> Resp {
    if let Some(header) = ignored {
        resp.headers.push(("x-dal-ignored".to_owned(), header));
    }
    resp
}

/// Builds the ignored-members header value, when members were ignored.
fn ignored_header(ignored: &[String]) -> Option<String> {
    if ignored.is_empty() {
        None
    } else {
        Some(ignored.join(","))
    }
}

/// Builds the shared harness state for one request.
fn harness_shared(ctx: &Arc<ServeCtx>) -> HarnessShared {
    HarnessShared {
        host: ctx.host.clone(),
        options: crate::serve::router_options(&ctx.cfg),
        digests: Arc::clone(&ctx.digests),
        shutdown: ctx.shutdown.clone(),
    }
}
