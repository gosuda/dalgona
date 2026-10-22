//! Turn inference: provider streams normalized to core events.
//!
//! The driver (later in this file) consumes normalized streams; the
//! converter below owns the decoding-level to recorded-level mapping.
//! Catalog fetches happen per request here; the driver caches the
//! resolved model across a turn's requests.

use std::collections::HashMap;
use std::sync::{Arc, Weak};

use dal_core::{
    BlobId, ContextItem, Family, InferFailure, ModelRequest, ModelRoute, Part, SessionId, Stop,
    StreamChannel, StreamEvent,
};
use dal_provider::{
    EventStream, ProviderError, StopReason, StreamEvent as ProviderEvent, ToolArgs,
};
use tokio_util::sync::CancellationToken;

use super::SessionHandle;
use crate::error::{AgentError, ServiceError};
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
    /// The continuation slot of the owning turn, invocation root, or relay
    /// stream, when one owns this request. Cursor steps persist through it;
    /// every other route ignores it. Absent for service-only inference and
    /// until its owner threads its handle.
    pub continuation: Option<Weak<super::continuation::ContinuationSlot>>,
}

/// Resolves one route without opening a provider stream or consuming a
/// scripted response.
pub(crate) async fn model_ready(
    deps: &RequestDeps,
    model: ModelRoute,
    cancel: &CancellationToken,
) -> Result<Option<ModelRoute>, ServiceError> {
    if cancel.is_cancelled() {
        return Err(ServiceError::Cancelled);
    }
    if let Some(found) = crate::ext::synthetic::find_route(&deps.host.shared, &model) {
        return Ok(Some(found.route()));
    }
    let host = Arc::clone(&deps.host);
    if host.shared.providers.scripted_provider().is_some() {
        return Ok(Some(model));
    }

    let aliases: Vec<(Box<str>, Box<str>)> = host
        .shared
        .config
        .aliases()
        .iter()
        .map(|(name, target)| (name.clone(), target.clone()))
        .collect();
    let runtime = tokio::runtime::Handle::current();
    let operation_cancel = cancel.child_token();
    let _cancel_on_drop = operation_cancel.clone().drop_guard();
    let check = tokio::task::spawn_blocking(move || {
        let catalog = runtime.block_on(async {
            tokio::select! {
                biased;
                () = operation_cancel.cancelled() => Err(ServiceError::Cancelled),
                result = host.shared.providers.catalog() => {
                    result.map_err(|error| ServiceError::failed(None, error.to_string()))
                }
            }
        })?;
        if operation_cancel.is_cancelled() {
            return Err(ServiceError::Cancelled);
        }
        let resolved = match dal_provider::resolve_route(&catalog, &aliases, &model) {
            Ok(resolved) => resolved,
            Err(
                dal_provider::ResolveError::UnknownModel { .. }
                | dal_provider::ResolveError::AmbiguousModel { .. },
            ) => return Ok(None),
            Err(error) => return Err(ServiceError::failed(None, error.to_string())),
        };
        let provider_id = resolved.provider.clone();
        let bedrock = matches!(
            resolved.route,
            ModelRoute::Api {
                family: Family::Bedrock,
                ..
            }
        );
        let provider = host.shared.providers.provider(resolved);
        if operation_cancel.is_cancelled() {
            return Err(ServiceError::Cancelled);
        }
        match provider {
            Ok(_) => {}
            Err(dal_provider::ProviderError::NoCredentials { .. }) => return Ok(None),
            Err(error) => return Err(ServiceError::failed(None, error.to_string())),
        }
        if bedrock {
            let available = runtime
                .block_on(
                    host.shared
                        .providers
                        .credential_source_available(&provider_id, &operation_cancel),
                )
                .map_err(|error| match error {
                    dal_provider::ProviderError::Cancelled => ServiceError::Cancelled,
                    error => ServiceError::failed(None, error.to_string()),
                })?;
            if !available {
                return Ok(None);
            }
        }
        Ok(Some(model))
    });
    let result = check
        .await
        .map_err(|error| ServiceError::failed(None, error.to_string()))?;
    if cancel.is_cancelled() {
        Err(ServiceError::Cancelled)
    } else {
        result
    }
}

/// Opens one provider stream for a model request.
///
/// Stored blobs are read back into the request first, so a registered
/// model, the scripted provider, and a live provider all receive inline
/// content. Scripted providers then bypass the catalog; live providers
/// resolve through one catalog fetch. Hydration and open failures return a
/// stream yielding the single error, so callers handle transport and
/// content uniformly.
pub(crate) async fn infer_stream(
    deps: &RequestDeps,
    mut req: ModelRequest,
    cancel: &CancellationToken,
) -> EventStream {
    if let Err(error) = hydrate_blobs(deps, &mut req, cancel).await {
        return failed(error);
    }
    if let Some(found) = crate::ext::synthetic::find_route(&deps.host.shared, &req.model) {
        return crate::ext::synthetic::open(deps, found, req, cancel).await;
    }
    match open_stream(deps, req, cancel).await {
        Ok(stream) => crate::ext::synthetic::observe(stream),
        Err(error) => failed(error),
    }
}

/// A stream that yields `error` and ends.
fn failed(error: ProviderError) -> EventStream {
    let items = vec![Err(error)];
    EventStream::new(futures::stream::iter(items), || {})
}

/// Replaces every stored blob of the request context with its content.
///
/// The session store moves content of 16 KiB or more out of the journal,
/// and the projection then names it by digest. Neither a provider nor a
/// registered model can read the store, so this is where those references
/// become inline images or text again, before any route is chosen. Order,
/// media types, and every other part are unchanged. A request that names
/// no stored image or text is left as it is: nothing is cloned, and
/// inline content is never copied.
///
/// A blob that is absent, unreadable, from a session that is not open, or
/// not the recorded length refuses the request: the content is never
/// skipped, and nothing is sent without it.
async fn hydrate_blobs(
    deps: &RequestDeps,
    req: &mut ModelRequest,
    cancel: &CancellationToken,
) -> Result<(), ProviderError> {
    let Some(first) = req
        .context
        .iter()
        .flat_map(|item| parts(item).iter())
        .find_map(stored_blob)
    else {
        return Ok(());
    };
    let Some(handle) = session_handle(deps) else {
        return Err(ProviderError::UnresolvedBlob { blob_id: first });
    };
    if cancel.is_cancelled() {
        return Err(cancelled());
    }
    // The blob path alone copies the message list; the no-blob path above
    // returns `req` untouched.
    let mut items = req.context.to_vec();
    for item in &mut items {
        for part in parts_mut(item) {
            hydrate_part(&handle, part, cancel).await?;
        }
    }
    req.context = items.into();
    Ok(())
}

/// The media type the projection gives an oversized text part.
///
/// `context::content_part` names a spilled `JournalPart::TextBlob` with this
/// type, so it is how a stored blob is recognized as text.
const TEXT_MIME: &str = "text/plain";

/// Whether a stored blob of this media type is text.
fn is_text(mime: &str) -> bool {
    mime == TEXT_MIME
}

/// Whether a stored blob of this media type returns to the request as
/// content: images as image bytes, plain text as text. Any other media type
/// keeps its digest.
fn is_hydrated(mime: &str) -> bool {
    is_text(mime) || mime.starts_with("image/")
}

/// Replaces `part` with the stored content when it names a stored image or
/// stored text.
///
/// An image returns as image bytes under its own media type. Text returns
/// as UTF-8 and refuses the request when the stored bytes are not valid
/// UTF-8: it is never sent with replacement characters.
async fn hydrate_part(
    handle: &SessionHandle,
    part: &mut Part,
    cancel: &CancellationToken,
) -> Result<(), ProviderError> {
    let Part::Blob {
        blob_id,
        mime,
        bytes: recorded,
    } = part
    else {
        return Ok(());
    };
    if !is_hydrated(mime) {
        return Ok(());
    }
    let id = *blob_id;
    let bytes = read_blob(handle, id, *recorded, cancel).await?;
    *part = if is_text(mime) {
        let text = String::from_utf8(bytes).map_err(|error| {
            fatal(format!(
                "text blob {id} does not hold valid UTF-8 ({error}); the text was not sent."
            ))
        })?;
        Part::Text {
            text: text.into_boxed_str(),
        }
    } else {
        Part::Image {
            mime: std::mem::take(mime),
            bytes: bytes.into_boxed_slice(),
        }
    };
    Ok(())
}

/// The digest behind a part that names a stored image or stored text.
fn stored_blob(part: &Part) -> Option<BlobId> {
    match part {
        Part::Blob { blob_id, mime, .. } if is_hydrated(mime) => Some(*blob_id),
        Part::Blob { .. } | Part::Text { .. } | Part::Image { .. } => None,
    }
}

/// The content parts of one context message; an assistant message has none.
fn parts(item: &ContextItem) -> &[Part] {
    match item {
        ContextItem::User { parts } | ContextItem::ToolResult { parts, .. } => parts.as_slice(),
        ContextItem::Assistant { .. } => &[],
    }
}

/// The content parts of one context message, for in-place replacement.
fn parts_mut(item: &mut ContextItem) -> &mut [Part] {
    match item {
        ContextItem::User { parts } | ContextItem::ToolResult { parts, .. } => parts.as_mut_slice(),
        ContextItem::Assistant { .. } => &mut [],
    }
}

/// The actor port of the request's session, while that session is open.
fn session_handle(deps: &RequestDeps) -> Option<SessionHandle> {
    deps.host
        .sessions
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .get(&deps.session)
        .map(|entry| entry.handle.clone())
}

/// Reads one stored blob through the session actor.
///
/// A cancelled request stops waiting at once. An absent digest is the typed
/// unresolved-blob refusal; every other failure, and a stored length that
/// differs from the journal's record, is a fatal inference failure naming
/// the blob.
async fn read_blob(
    handle: &SessionHandle,
    id: BlobId,
    recorded: u64,
    cancel: &CancellationToken,
) -> Result<Vec<u8>, ProviderError> {
    let read = tokio::select! {
        biased;
        () = cancel.cancelled() => return Err(cancelled()),
        read = handle.blob(id) => read,
    };
    match read {
        Ok(bytes) if u64::try_from(bytes.len()).is_ok_and(|length| length == recorded) => Ok(bytes),
        Ok(bytes) => Err(fatal(format!(
            "blob {id} holds {} bytes but its journal part records {recorded}; the blob was not sent.",
            bytes.len()
        ))),
        Err(AgentError::BlobNotFound { .. }) => Err(ProviderError::UnresolvedBlob { blob_id: id }),
        Err(error) => Err(fatal(format!(
            "blob {id} could not be read from the session store: {error}"
        ))),
    }
}

/// The typed failure of a request cancelled before it was sent.
fn cancelled() -> ProviderError {
    ProviderError::Synthetic(InferFailure::Cancelled)
}

/// A terminal request failure with no next-action text.
fn fatal(message: String) -> ProviderError {
    ProviderError::Synthetic(InferFailure::Fatal {
        message: message.into(),
        fix: None,
    })
}

/// Opens the provider stream for one request.
///
/// A live provider binds one concrete API route, so a request that names its
/// model another way (a catalog id such as `openai/gpt-6-luna`, which parses
/// as a synthetic id, or a configured alias) opens under the route the
/// catalog resolved it to. A request already on that route is sent as it is.
async fn open_stream(
    deps: &RequestDeps,
    mut req: ModelRequest,
    cancel: &CancellationToken,
) -> Result<EventStream, dal_provider::ProviderError> {
    let providers = &deps.host.shared.providers;
    if let Some(script) = providers.scripted_provider() {
        return script
            .open(deps.session, &req, &[], Arc::new(|_: String| {}), cancel)
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
    let rebound = (resolved.route != req.model).then(|| resolved.route.clone());
    let provider = providers.provider(resolved)?;
    if let Some(route) = rebound {
        req.model = route;
    }
    if matches!(&req.model, ModelRoute::Api { family, .. } if *family == Family::Cursor) {
        return super::continuation::open_step(deps, provider, &req, cancel).await;
    }
    provider
        .open(deps.session, &req, &[], Arc::new(|_: String| {}), cancel)
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
            In::MessageReplay { capture } => {
                vec![StreamEvent::MessageReplay { capture }]
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
