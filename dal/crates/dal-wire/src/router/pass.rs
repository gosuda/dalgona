//! Pass-through relay for the HTTP router.
//!
//! Non-harness models relay to the provider through `Host::relay` with no
//! journal turn: the client runs its own tools. Thinking replay survives
//! only when the route and wire families match (both Anthropic or both
//! Responses). Provider statuses before the first event map to 429/503/502;
//! anything else is a 502 `upstream provider failed` failure. Streaming
//! relays encode each provider event as it arrives; a failure after the
//! response started is the family's stream failure event.

use dal_core::{Family, ModelRequest, ModelRoute};
use dal_provider::{EventStream, ProviderError, StreamEvent};
use sonic_rs::Value;
use tokio_util::sync::CancellationToken;

use super::decode::RouterFail;
use super::harness::{HarnessEvent, HarnessRequest, HarnessShared, HarnessStop};

mod context;
mod live;

pub(crate) use live::RelayMapper;

/// The outcome of one relayed turn.
pub(crate) struct RelayTurn {
    /// The accumulated text.
    pub text: String,
    /// The accumulated reasoning.
    pub reasoning: String,
    /// The tool calls made.
    pub calls: Vec<RelayCall>,
    /// The terminal stop.
    pub stop: HarnessStop,
    /// Input tokens.
    pub input_tokens: u64,
    /// Output tokens.
    pub output_tokens: u64,
    /// Provider thinking-replay items preserved for a same-family wire.
    pub replay: Vec<Value>,
}

/// One relayed tool call.
#[derive(Clone, Debug)]
pub(crate) struct RelayCall {
    /// The call id.
    pub call: String,
    /// The tool name.
    pub name: String,
    /// The arguments JSON.
    pub args: String,
}

impl RelayTurn {
    /// Folds one mapped relay event into the turn.
    fn absorb(&mut self, event: HarnessEvent) {
        match event {
            HarnessEvent::Text(text) => self.text.push_str(&text),
            HarnessEvent::Reasoning(text) => self.reasoning.push_str(&text),
            HarnessEvent::ToolCallDone { id, name, args } => self.calls.push(RelayCall {
                call: id,
                name,
                args,
            }),
            HarnessEvent::Replay(item) => self.replay.push(item),
            HarnessEvent::Usage(usage) => {
                self.input_tokens = usage.input;
                self.output_tokens = usage.output;
            }
            HarnessEvent::Stop(stop) => self.stop = stop,
            HarnessEvent::Started(_)
            | HarnessEvent::ToolCallStarted { .. }
            | HarnessEvent::ToolArgs { .. } => {}
        }
    }
}

/// An open relay whose provider produced its first event.
pub(crate) struct Relay {
    stream: EventStream,
    first: StreamEvent,
    mapper: RelayMapper,
}

/// Opens one relay: checks credentials, builds the provider request, and
/// waits for the first provider event so early provider failures keep their
/// HTTP status.
pub(crate) async fn open(
    shared: &HarnessShared,
    route: &ModelRoute,
    request: &HarnessRequest,
) -> Result<Relay, RouterFail> {
    check_relay_credentials(shared, route)?;
    let (provider_request, wire) = match request {
        HarnessRequest::Chat(req) => (context::chat_request(route, req)?, Family::Chat),
        HarnessRequest::Responses(req) => {
            (context::responses_request(route, req)?, Family::Responses)
        }
        HarnessRequest::Messages(req) => {
            (context::messages_request(route, req)?, Family::Anthropic)
        }
    };
    let stream = start_stream(shared, route, provider_request).await?;
    Relay::start(
        stream,
        RelayMapper::new(api_family(route), wire),
        &shared.shutdown,
    )
    .await
}

/// Starts one provider stream through the host.
async fn start_stream(
    shared: &HarnessShared,
    route: &ModelRoute,
    request: ModelRequest,
) -> Result<EventStream, RouterFail> {
    shared
        .host
        .relay(
            crate::protocol::mint_client_id("router"),
            route.clone(),
            request,
        )
        .await
        .map_err(|error| host_fail(&error))
}

/// Maps a host failure to open a relay to its router failure.
fn host_fail(error: &dal_agent::HostError) -> RouterFail {
    let (status, code) = match error {
        dal_agent::HostError::NotFound { .. } => (404, "model_not_found"),
        dal_agent::HostError::Admission { .. } | dal_agent::HostError::Closed => {
            (503, "overloaded")
        }
        _ => (502, "upstream_failed"),
    };
    RouterFail {
        status,
        code,
        message: error.to_string(),
    }
}

impl Relay {
    /// Waits for the first provider event of an open stream.
    pub(crate) async fn start(
        mut stream: EventStream,
        mapper: RelayMapper,
        shutdown: &CancellationToken,
    ) -> Result<Self, RouterFail> {
        let first = next_event(&mut stream, shutdown).await?;
        Ok(Self {
            stream,
            first,
            mapper,
        })
    }

    /// Collects the whole relayed turn for a non-stream reply.
    pub(crate) async fn collect(
        self,
        shutdown: &CancellationToken,
    ) -> Result<RelayTurn, RouterFail> {
        let Self {
            mut stream,
            first,
            mut mapper,
        } = self;
        let mut turn = RelayTurn {
            text: String::new(),
            reasoning: String::new(),
            calls: Vec::new(),
            stop: HarnessStop::EndTurn,
            input_tokens: 0,
            output_tokens: 0,
            replay: Vec::new(),
        };
        let mut events = Vec::new();
        let mut event = first;
        loop {
            let terminal = matches!(event, StreamEvent::Stop { .. });
            mapper.map(event, &mut events);
            for mapped in events.drain(..) {
                turn.absorb(mapped);
            }
            if terminal {
                break;
            }
            event = next_event(&mut stream, shutdown).await?;
        }
        if let HarnessStop::Failed(message) = turn.stop {
            return Err(RouterFail {
                status: 502,
                code: "upstream_failed",
                message,
            });
        }
        Ok(turn)
    }
}

/// Waits for the next provider event; a stopping listener or a provider
/// failure ends the wait with its router failure.
async fn next_event(
    stream: &mut EventStream,
    shutdown: &CancellationToken,
) -> Result<StreamEvent, RouterFail> {
    let item = tokio::select! {
        biased;
        () = shutdown.cancelled() => {
            return Err(RouterFail {
                status: 503,
                code: "overloaded",
                message: "the server is stopping".to_owned(),
            });
        }
        item = stream.next() => item,
    };
    match item {
        Some(Ok(event)) => Ok(event),
        Some(Err(error)) => Err(provider_fail(error)),
        None => Err(provider_fail(ProviderError::StreamCut)),
    }
}

/// Returns the API family of one route, when it has one.
fn api_family(route: &ModelRoute) -> Option<Family> {
    match route {
        ModelRoute::Api { family, .. } => Some(*family),
        ModelRoute::Synthetic { .. } | ModelRoute::Harness { .. } => None,
    }
}

/// Maps a provider failure to its router failure.
fn provider_fail(error: ProviderError) -> RouterFail {
    match error {
        ProviderError::Status {
            status: 429,
            message,
            ..
        } => RouterFail {
            status: 429,
            code: "rate_limit",
            message,
        },
        ProviderError::Status {
            status: 529 | 503,
            message,
            ..
        } => RouterFail {
            status: 503,
            code: "overloaded",
            message,
        },
        ProviderError::Status { message, .. } => RouterFail {
            status: 502,
            code: "upstream_failed",
            message: format!("upstream provider failed: {message}"),
        },
        error => RouterFail {
            status: 502,
            code: "upstream_failed",
            message: format!("upstream provider failed: {error}"),
        },
    }
}

/// Checks relay credentials for one route before relaying.
fn check_relay_credentials(shared: &HarnessShared, route: &ModelRoute) -> Result<(), RouterFail> {
    if matches!(
        route,
        ModelRoute::Synthetic { .. } | ModelRoute::Harness { .. }
    ) {
        return Ok(());
    }
    if let Some(provider) = crate::rpc::missing_credential(&shared.host, route) {
        return Err(RouterFail::unauthorized(format!(
            "{provider} has no credentials: auth.json has no entry for it"
        )));
    }
    Ok(())
}
