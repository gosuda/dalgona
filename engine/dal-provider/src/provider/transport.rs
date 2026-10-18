//! Provider transport helpers: error decoding, redaction, permits, and refresh.

use std::{
    pin::Pin,
    sync::{Arc, Mutex},
};

use dal_core::{Family, Usage};
use futures::{Stream, StreamExt, future::ready};
use serde::Deserialize;
use tokio::sync::OwnedSemaphorePermit;
use tokio_util::sync::CancellationToken;

use crate::{
    auth::{
        credential::{Credential, OAuthCredential},
        refresh::{RefreshReason, Refresher},
    },
    error::{ProviderError, UsageCheckReason},
    family::{
        self,
        google::shared::{ResponseIdSlot, decode_events as decode_google_events},
    },
    provider::{ProviderDef, ProviderEntry},
    stream::{EventStream, StreamEvent},
};

#[derive(Default, Deserialize)]
struct ErrorBody {
    error: Option<ErrorDetail>,
    message: Option<String>,
    detail: Option<String>,
    code: Option<String>,
    #[serde(rename = "type")]
    kind: Option<String>,
}

#[derive(Default, Deserialize)]
struct ErrorDetail {
    code: Option<String>,
    #[serde(rename = "type")]
    kind: Option<String>,
    message: Option<String>,
}

pub(crate) fn error_fields(body: &str) -> (Option<String>, String) {
    let parsed = sonic_rs::from_str::<ErrorBody>(body).ok();
    let code = parsed.as_ref().and_then(|body| {
        body.error
            .as_ref()
            .and_then(|error| error.code.clone().or_else(|| error.kind.clone()))
            .or_else(|| body.code.clone().or_else(|| body.kind.clone()))
    });
    let message = parsed
        .and_then(|body| {
            body.error
                .and_then(|error| error.message)
                .or(body.message)
                .or(body.detail)
        })
        .unwrap_or_else(|| body.lines().next().unwrap_or_default().to_owned());
    (code, first_line(&message, 300))
}

pub(crate) fn first_line(input: &str, max_bytes: usize) -> String {
    let line = input.lines().next().unwrap_or_default();
    let mut end = line.len().min(max_bytes);
    while !line.is_char_boundary(end) {
        end -= 1;
    }
    line[..end].to_owned()
}

/// A single-error stream for a family whose responses never reach the SSE
/// decoder: its own adapter owns the wire and decodes it itself, so no
/// request path hands such a response to [`decode_response`].
fn dedicated_decoder(
    family: Family,
) -> Pin<Box<dyn Stream<Item = Result<StreamEvent, ProviderError>> + Send>> {
    Box::pin(futures::stream::iter([
        Result::<StreamEvent, ProviderError>::Err(ProviderError::InvalidRequest {
            message: format!(
                "{} responses decode through their own adapter",
                crate::error::family_label(family)
            ),
        }),
    ]))
}

pub(crate) fn decode_response(
    response: reqwest::Response,
    family: Family,
    provider: &str,
    model: &str,
    oauth: bool,
    replay_prefix: Option<&str>,
    secrets: Vec<Box<str>>,
    response_id: Option<ResponseIdSlot>,
) -> EventStream {
    let read_failure: Arc<Mutex<Option<ProviderError>>> = Arc::default();
    let chunks = response
        .bytes_stream()
        .scan(Arc::clone(&read_failure), move |slot, chunk| {
            ready(match chunk {
                Ok(bytes) => Some(bytes.to_vec()),
                Err(error) => {
                    // An idle stall stays a cut, like the Codex HTTPS stream.
                    if !error.is_timeout()
                        && let Ok(mut failure) = slot.lock()
                    {
                        *failure = Some(crate::http::from_reqwest(family, error));
                    }
                    None
                }
            })
        });
    // Bedrock event-stream frames, Devin Connect frames, and Ollama NDJSON
    // records are byte grammars, not SSE: they decode straight off the chunk
    // stream, ahead of the SSE wrap.
    if matches!(family, Family::Bedrock | Family::Devin | Family::Ollama) {
        let decoded: Pin<Box<dyn Stream<Item = Result<StreamEvent, ProviderError>> + Send>> =
            match family {
                Family::Bedrock => {
                    Box::pin(family::bedrock::decode_stream(chunks, provider, model))
                }
                Family::Devin => Box::pin(family::devin::decode::decode_chunks(
                    chunks, provider, model,
                )),
                _ => Box::pin(family::ollama::decode_stream(
                    chunks,
                    family::ollama::OllamaDecoder::new(),
                )),
            };
        return guard_errors(decoded, read_failure, secrets);
    }
    let events = crate::sse::decode_stream(chunks);
    let decoded: Pin<Box<dyn Stream<Item = Result<StreamEvent, ProviderError>> + Send>> =
        match family {
            Family::Chat => Box::pin(family::chat::decode_events(
                events,
                family::chat::ChatDecoder::new(provider, model),
            )),
            Family::Responses | Family::Codex => Box::pin(family::responses::decode(
                events,
                family,
                Box::<str>::from(model),
            )),
            Family::Anthropic => Box::pin(family::anthropic::decode_stream(
                events,
                model.into(),
                oauth,
                replay_prefix,
            )),
            // Bedrock, Devin and Ollama decoded ahead of the SSE wrap above;
            // Cursor owns its run transport, Droid the proxy lifecycle
            // (`family::droid::open`), GitLab its proxy and workflow
            // adapters, and Gemini CLI its subprocess. None of them hands a
            // response to this function.
            Family::Bedrock
            | Family::Devin
            | Family::Ollama
            | Family::Droid
            | Family::Cursor
            | Family::GitLab
            | Family::GoogleGeminiCli => dedicated_decoder(family),
            Family::GoogleGenerativeAi | Family::GoogleVertex | Family::GoogleAntigravity => {
                Box::pin(decode_google_events(
                    family,
                    provider,
                    model,
                    events,
                    response_id,
                ))
            }
            Family::Mistral => Box::pin(family::mistral::decode_events(
                events,
                family::mistral::MistralDecoder::new(provider, model),
            )),
            Family::PiMessages => Box::pin(family::pi_messages::decode_events(
                events,
                family::pi_messages::PiMessagesDecoder::new(provider, model),
            )),
        };
    guard_errors(decoded, read_failure, secrets)
}

fn guard_errors(
    decoded: Pin<Box<dyn Stream<Item = Result<StreamEvent, ProviderError>> + Send>>,
    read_failure: Arc<Mutex<Option<ProviderError>>>,
    secrets: Vec<Box<str>>,
) -> EventStream {
    // A body read failure ends the byte source, so the decoder would report
    // it as a cut (or as a protocol error for a half-read frame); the transport
    // failure is the retryable truth.
    let safe_errors = decoded.map(move |result| {
        result.map_err(|error| {
            let failure = read_failure.lock().ok().and_then(|mut slot| slot.take());
            redact_provider_error(failure.unwrap_or(error), &secrets)
        })
    });
    EventStream::new(safe_errors, || {})
}

pub(crate) fn redact_provider_error(error: ProviderError, secrets: &[Box<str>]) -> ProviderError {
    let redact = |text: String| redact_text(text, secrets);
    match error {
        ProviderError::Transport { family, reason } => ProviderError::Transport {
            family,
            reason: redact(reason),
        },
        ProviderError::Status {
            family,
            status,
            message,
        } => ProviderError::Status {
            family,
            status,
            message: redact(message),
        },
        ProviderError::InvalidRequest { message } => ProviderError::InvalidRequest {
            message: redact(message),
        },
        ProviderError::ContextOverflow {
            family,
            code,
            message,
        } => ProviderError::ContextOverflow {
            family,
            code: redact(code),
            message: redact(message),
        },
        ProviderError::RateLimited {
            message,
            retry_after,
        } => ProviderError::RateLimited {
            message: redact(message),
            retry_after,
        },
        ProviderError::RetryAfterTooLong { seconds, message } => ProviderError::RetryAfterTooLong {
            seconds,
            message: redact(message),
        },
        ProviderError::Quota { message } => ProviderError::Quota {
            message: redact(message),
        },
        ProviderError::WsClosed { code } => ProviderError::WsClosed {
            code: code.map(|(code, reason)| (code, redact(reason))),
        },
        ProviderError::Protocol { family, detail } => ProviderError::Protocol {
            family,
            detail: redact(detail),
        },
        ProviderError::UsageLimit { model, message } => ProviderError::UsageLimit {
            model: redact(model),
            message: redact(message),
        },
        ProviderError::UsageNotIncluded { message } => ProviderError::UsageNotIncluded {
            message: redact(message),
        },
        ProviderError::ReserveUnavailable { status, message } => {
            ProviderError::ReserveUnavailable {
                status,
                message: redact(message),
            }
        }
        ProviderError::AuthFileInvalid { path, message } => ProviderError::AuthFileInvalid {
            path,
            message: redact(message),
        },
        ProviderError::AuthWrite { reason } => ProviderError::AuthWrite {
            reason: redact(reason),
        },
        ProviderError::CallbackBind { port, reason } => ProviderError::CallbackBind {
            port,
            reason: redact(reason),
        },
        ProviderError::TokenExchange { status, message } => ProviderError::TokenExchange {
            status,
            message: redact(message),
        },
        ProviderError::DeviceCode { status, message } => ProviderError::DeviceCode {
            status,
            message: redact(message),
        },
        ProviderError::UsageCheck { reason } => ProviderError::UsageCheck {
            reason: redact_usage_reason(reason, secrets),
        },
        other => other,
    }
}

pub(crate) fn redact_text(mut text: String, secrets: &[Box<str>]) -> String {
    for secret in secrets {
        if !secret.is_empty() && text.contains(secret.as_ref()) {
            text = text.replace(secret.as_ref(), "<redacted>");
        }
    }
    text
}

fn redact_usage_reason(reason: UsageCheckReason, secrets: &[Box<str>]) -> UsageCheckReason {
    let redact = |text: String| redact_text(text, secrets);
    match reason {
        UsageCheckReason::Status { status, message } => UsageCheckReason::Status {
            status,
            message: redact(message),
        },
        UsageCheckReason::Transport { reason } => UsageCheckReason::Transport {
            reason: redact(reason),
        },
        other => other,
    }
}
pub(crate) async fn acquire_provider_permit(
    owner: &super::set::ProviderSetInner,
    provider: &str,
    cancel: &CancellationToken,
) -> Result<OwnedSemaphorePermit, ProviderError> {
    {
        let slot = owner
            .providers
            .get(provider)
            .ok_or_else(|| ProviderError::InvalidRequest {
                message: format!("provider {provider} is not configured"),
            })?;
        let family = slot.entry.family;
        tokio::select! {
            biased;
            () = cancel.cancelled() => Err(ProviderError::Transport {
                family,
                reason: String::from("provider request cancelled"),
            }),
            permit = Arc::clone(&slot.permits).acquire_owned() => permit
                .map_err(|_| ProviderError::Transport {
                    family,
                    reason: String::from("provider request admission is closed"),
                }),
        }
    }
}

struct PremiumState {
    stream: EventStream,
    observed: bool,
    pending_stop: Option<StreamEvent>,
}

fn stamped_usage(premium: f64, mut usage: Usage) -> StreamEvent {
    usage.premium_requests = Some(premium);
    StreamEvent::Usage { usage }
}

fn premium_only_usage(premium: f64) -> StreamEvent {
    StreamEvent::Usage {
        usage: Usage {
            input_tokens: 0,
            cached_input_tokens: 0,
            output_tokens: 0,
            reasoning_tokens: None,
            cache_write_tokens: 0,
            premium_requests: Some(premium),
            cost_usd: None,
        },
    }
}

/// Stamps the premium count of one completed Copilot attempt onto its stream.
///
/// Every observed usage carries the count. A successful stop without an
/// observed usage gains a premium-only usage immediately before it. Errors
/// and cuts pass through untouched, so failed or cancelled attempts never
/// acquire a successful completed count.
fn stamp_item(
    mut state: PremiumState,
    premium: f64,
    next: Result<StreamEvent, ProviderError>,
) -> (Result<StreamEvent, ProviderError>, PremiumState) {
    match next {
        Err(error) => (Err(error), state),
        Ok(StreamEvent::Usage { usage }) => {
            state.observed = true;
            (Ok(stamped_usage(premium, usage)), state)
        }
        Ok(StreamEvent::Stop { reason }) if state.observed => {
            (Ok(StreamEvent::Stop { reason }), state)
        }
        Ok(StreamEvent::Stop { reason }) => {
            state.observed = true;
            state.pending_stop = Some(StreamEvent::Stop { reason });
            (Ok(premium_only_usage(premium)), state)
        }
        Ok(event) => (Ok(event), state),
    }
}

pub(crate) fn stamp_premium(stream: EventStream, premium: f64) -> EventStream {
    let state = PremiumState {
        stream,
        observed: false,
        pending_stop: None,
    };
    let source = futures::stream::unfold(state, move |mut state| async move {
        if let Some(stop) = state.pending_stop.take() {
            return Some((Ok(stop), state));
        }
        let next = state.stream.next().await?;
        Some(stamp_item(state, premium, next))
    });
    EventStream::new(source, || {})
}

pub(crate) fn hold_permit(stream: EventStream, permit: OwnedSemaphorePermit) -> EventStream {
    let source = futures::stream::unfold(
        (stream, Some(permit)),
        |(mut stream, mut permit)| async move {
            match stream.next().await {
                Some(Ok(event @ StreamEvent::Stop { .. })) => {
                    drop(permit.take());
                    Some((Ok(event), (stream, permit)))
                }
                Some(Err(error)) => {
                    drop(permit.take());
                    Some((Err(error), (stream, permit)))
                }
                Some(item) => Some((item, (stream, permit))),
                None => None,
            }
        },
    );
    EventStream::new(source, || {})
}

pub(crate) async fn refresh_expiring(
    refresher: &Refresher,
    entry: &ProviderEntry,
    credential: &Credential,
    cancel: &CancellationToken,
) -> Result<Credential, ProviderError> {
    let Credential::OAuth(held) = credential else {
        return Ok(credential.clone());
    };
    let Some(def) = entry.def.filter(|def| def.oauth.is_some()) else {
        return Ok(credential.clone());
    };
    tokio::select! {
        biased;
        () = cancel.cancelled() => Err(ProviderError::Transport {
            family: entry.family,
            reason: String::from("provider request cancelled while refreshing credentials"),
        }),
        credential = refresher.refresh(def, held, RefreshReason::Expiring) => credential,
    }
}
pub(crate) async fn refresh_credential(
    refresher: Arc<Refresher>,
    provider: Box<str>,
    def: Option<&'static ProviderDef>,
    held: OAuthCredential,
) -> Result<Credential, ProviderError> {
    let Some(def) = def.filter(|def| def.oauth.is_some()) else {
        return Err(ProviderError::SignInExpired {
            provider: provider.to_string(),
        });
    };
    refresher.refresh(def, &held, RefreshReason::Rejected).await
}
