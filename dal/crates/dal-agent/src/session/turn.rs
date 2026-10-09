//! Turn inference: provider streams normalized to core events.
//!
//! The driver (later in this file) consumes normalized streams; the
//! converter below owns the decoding-level to recorded-level mapping.
//! Catalog fetches happen per request here; the driver caches the
//! resolved model across a turn's requests.

use std::collections::HashMap;
use std::sync::Arc;

use dal_core::{ModelRequest, SessionId, Stop, StreamChannel, StreamEvent};
use dal_provider::{EventStream, StopReason, StreamEvent as ProviderEvent, ToolArgs};
use tokio_util::sync::CancellationToken;

use crate::host::HostState;

/// Session inputs for one provider request.
#[derive(Clone)]
pub(crate) struct RequestDeps {
    /// The owning session.
    pub session: SessionId,
    /// The host state for providers and admission.
    pub host: Arc<HostState>,
    /// The turn's script host, absent for relay and service-only inference.
    pub script: Option<Arc<super::script::SessionScriptHost>>,
}

/// Opens one provider stream for a model request.
///
/// Scripted providers bypass the catalog; live providers resolve through
/// one catalog fetch. Open failures return a stream yielding the single
/// error, so callers handle transport and content uniformly.
pub(crate) async fn infer_stream(
    deps: &RequestDeps,
    req: ModelRequest,
    cancel: &CancellationToken,
) -> EventStream {
    if let Some(found) = crate::ext::synthetic::find_route(&deps.host.shared, &req.model) {
        return crate::ext::synthetic::open(deps, found, req, cancel).await;
    }
    match open_stream(deps, &req, cancel).await {
        Ok(stream) => crate::ext::synthetic::observe(stream),
        Err(error) => {
            let items = vec![Err(error)];
            EventStream::new(futures::stream::iter(items), || {})
        }
    }
}

/// Opens the provider stream for one request.
async fn open_stream(
    deps: &RequestDeps,
    req: &ModelRequest,
    cancel: &CancellationToken,
) -> Result<EventStream, dal_provider::ProviderError> {
    let providers = &deps.host.shared.providers;
    if let Some(script) = providers.scripted_provider() {
        return script
            .open(deps.session, req, &[], Arc::new(|_: String| {}), cancel)
            .await;
    }
    let catalog = providers.catalog().await?;
    let aliases: Vec<(Box<str>, Box<str>)> = deps
        .host
        .shared
        .config
        .aliases()
        .iter()
        .map(|(name, target)| (name.clone(), target.clone()))
        .collect();
    let resolved =
        dal_provider::resolve_route(&catalog, &aliases, &req.model).map_err(|error| {
            dal_provider::ProviderError::InvalidRequest {
                message: error.to_string(),
            }
        })?;
    let provider = providers.provider(resolved)?;
    provider
        .open(deps.session, req, &[], Arc::new(|_: String| {}), cancel)
        .await
}

/// Normalizes decoding-level provider events to recorded core events.
///
/// Argument fragments may split UTF-8 characters, so per-call bytes
/// buffer here and only complete prefixes publish as text.
pub(crate) struct StreamConverter {
    /// Call id to (tool name, pending bytes).
    pending: HashMap<String, (String, Vec<u8>)>,
}

impl StreamConverter {
    /// An empty converter with no buffered calls.
    pub(crate) fn new() -> Self {
        Self {
            pending: HashMap::new(),
        }
    }

    /// Converts one provider event to zero or more core events.
    pub(crate) fn convert(&mut self, event: ProviderEvent) -> Vec<StreamEvent> {
        use ProviderEvent as In;
        match event {
            In::TextDelta { text } => vec![StreamEvent::Delta {
                channel: StreamChannel::Text,
                text: text.into(),
            }],
            In::ReasoningDelta { text } => vec![StreamEvent::Delta {
                channel: StreamChannel::Thinking,
                text: text.into(),
            }],
            In::ToolCallStarted { id, name } => {
                self.pending.entry(id).or_insert((name, Vec::new()));
                Vec::new()
            }
            In::ToolArgsDelta { id, fragment } => self.push_args(&id, &fragment),
            In::Replay { payload } => vec![StreamEvent::ThinkingReplay {
                payload: payload.item,
            }],
            In::ToolCallsDone { calls } => {
                let mut out = Vec::with_capacity(calls.len());
                for call in calls {
                    self.pending.remove(&call.id);
                    let args = match call.args {
                        ToolArgs::Parsed(args) => args,
                        ToolArgs::Invalid { .. } | ToolArgs::Truncated => dal_core::RawJson::null(),
                    };
                    out.push(StreamEvent::ToolCall {
                        call: dal_core::CallId::new(call.id),
                        name: call.name.into(),
                        args,
                    });
                }
                out
            }
            In::Usage { usage } => vec![StreamEvent::Usage(usage)],
            In::Stop { reason } => vec![StreamEvent::Stop(map_stop(&reason))],
            // `ProviderEvent` is `#[non_exhaustive]`; every known variant has
            // an explicit arm above. This arm covers only provider variants
            // added after this mapping: with no core event for an unknown
            // decoding-level event it maps to nothing, and a new variant
            // must gain an explicit arm with a mapping review.
            _ => {
                debug_assert!(false, "unmapped provider stream event; add an explicit arm");
                Vec::new()
            }
        }
    }

    /// Buffers one argument fragment, emitting complete UTF-8 prefixes.
    fn push_args(&mut self, id: &str, fragment: &[u8]) -> Vec<StreamEvent> {
        let Some((name, bytes)) = self.pending.get_mut(id) else {
            return Vec::new();
        };
        bytes.extend_from_slice(fragment);
        let valid = valid_prefix_len(bytes);
        if valid == 0 {
            return Vec::new();
        }
        let text: Box<str> = String::from_utf8_lossy(&bytes[..valid]).into();
        bytes.drain(..valid);
        vec![StreamEvent::Delta {
            channel: StreamChannel::ToolArgs {
                tool: name.clone().into(),
            },
            text,
        }]
    }

    /// Flushes buffered bytes as closing text fragments.
    pub(crate) fn flush(&mut self) -> Vec<StreamEvent> {
        let mut out = Vec::new();
        for (_, (name, bytes)) in std::mem::take(&mut self.pending) {
            if bytes.is_empty() {
                continue;
            }
            out.push(StreamEvent::Delta {
                channel: StreamChannel::ToolArgs { tool: name.into() },
                text: String::from_utf8_lossy(&bytes).into(),
            });
        }
        out
    }
}

/// Maps a provider stop reason to its recorded stop.
fn map_stop(reason: &StopReason) -> Stop {
    match reason {
        StopReason::EndTurn | StopReason::ToolUse => Stop::EndTurn,
        StopReason::MaxTokens => Stop::Length,
        StopReason::Refusal => Stop::Filter,
        StopReason::Paused | StopReason::Other(_) => Stop::Failed,
    }
}

/// Returns the length of the longest valid UTF-8 prefix of `bytes`.
fn valid_prefix_len(bytes: &[u8]) -> usize {
    let mut end = bytes.len();
    while end > 0 {
        if std::str::from_utf8(&bytes[..end]).is_ok() {
            return end;
        }
        end -= 1;
    }
    0
}
